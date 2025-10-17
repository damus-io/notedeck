use std::{
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::Fraction;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video::{VideoFormat, VideoFrameExt, VideoFrameRef, VideoInfo};
use tracing::{debug, trace, warn};

const BUS_POLL_TIMEOUT_MS: u64 = 250;
const MAX_DEBUG_FRAMES: usize = 5;
const BLANK_FRAME_THRESHOLD: u32 = 30;
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_STALL_RESTARTS: u32 = 3;
const STALL_RESTART_BACKOFF: Duration = Duration::from_secs(2);

static GST_INIT: OnceLock<Result<(), String>> = OnceLock::new();
static FRAME_DEBUG_LOG_COUNT: AtomicUsize = AtomicUsize::new(0);

fn ensure_gstreamer() -> Result<()> {
    match GST_INIT
        .get_or_init(|| {
            gst::init().map_err(|err| format!("GStreamer initialization failed: {err}"))
        })
        .clone()
    {
        Ok(()) => Ok(()),
        Err(msg) => Err(anyhow!(msg)),
    }
}

#[derive(Clone, Debug)]
pub struct DecodedFrame {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
    pub version: u64,
}

#[derive(Default)]
struct SharedState {
    frame: Option<DecodedFrame>,
    frame_version: u64,
    last_error: Option<String>,
    blank_frames: u32,
    needs_restart: bool,
    ever_had_frame: bool,
    first_frame_deadline: Option<Instant>,
}

pub struct LivestreamPlayer {
    pipeline: Option<gst::Element>,
    appsink: Option<gst_app::AppSink>,
    shared: Arc<Mutex<SharedState>>,
    bus_thread: Option<thread::JoinHandle<()>>,
    stop_flag: Option<Arc<AtomicBool>>,
    current_uri: Option<String>,
    restart_attempts: u32,
    last_restart: Option<Instant>,
}

impl LivestreamPlayer {
    pub fn new() -> Result<Self> {
        ensure_gstreamer()?;

        Ok(Self {
            pipeline: None,
            appsink: None,
            shared: Arc::new(Mutex::new(SharedState::default())),
            bus_thread: None,
            stop_flag: None,
            current_uri: None,
            restart_attempts: 0,
            last_restart: None,
        })
    }

    pub fn play(&mut self, uri: &str) -> Result<()> {
        ensure_gstreamer()?;

        if self.current_uri.as_deref() == Some(uri) {
            if let Some(pipeline) = &self.pipeline {
                if pipeline.current_state() != gst::State::Playing {
                    pipeline
                        .set_state(gst::State::Playing)
                        .context("Unable to resume playback")?;
                }
                return Ok(());
            }
        } else {
            self.stop();
        }

        self.restart_attempts = 0;
        self.last_restart = None;
        self.build_pipeline(uri)
    }

    fn restart(&mut self) -> Result<()> {
        let uri = match self.current_uri.clone() {
            Some(uri) => uri,
            None => return Ok(()),
        };

        self.stop();
        ensure_gstreamer()?;
        self.build_pipeline(&uri)
    }

    fn build_pipeline(&mut self, uri: &str) -> Result<()> {
        {
            let mut state = self.shared.lock().expect("player state poisoned");
            *state = SharedState::default();
            state.first_frame_deadline = Some(Instant::now() + FIRST_FRAME_TIMEOUT);
        }

        let video_caps = gst::Caps::builder("video/x-raw")
            .features(["memory:SystemMemory"])
            .field("format", &"RGBA")
            .field("interlace-mode", &"progressive")
            .field("pixel-aspect-ratio", &Fraction::new(1, 1))
            .build();

        let shared = Arc::clone(&self.shared);
        let appsink = gst_app::AppSink::builder().caps(&video_caps).build();

        appsink.set_max_buffers(3);
        appsink.set_drop(false);
        appsink.set_property("sync", &true);

        let sink_element: gst::Element = appsink.clone().upcast();
        let queue = gst::ElementFactory::make("queue")
            .build()
            .map_err(|err| anyhow!("GStreamer element 'queue' is not available: {err}"))?;
        let deinterlace = gst::ElementFactory::make("deinterlace")
            .build()
            .map_err(|err| anyhow!("GStreamer element 'deinterlace' is not available: {err}"))?;
        let convert = gst::ElementFactory::make("videoconvert")
            .build()
            .map_err(|err| anyhow!("GStreamer element 'videoconvert' is not available: {err}"))?;
        let scale = gst::ElementFactory::make("videoscale")
            .build()
            .map_err(|err| anyhow!("GStreamer element 'videoscale' is not available: {err}"))?;
        let capsfilter = gst::ElementFactory::make("capsfilter")
            .build()
            .map_err(|err| anyhow!("GStreamer element 'capsfilter' is not available: {err}"))?;
        capsfilter.set_property("caps", &video_caps);

        let video_bin = gst::Bin::with_name("notedeck-video-bin");
        video_bin
            .add_many(&[
                &queue,
                &deinterlace,
                &convert,
                &scale,
                &capsfilter,
                &sink_element,
            ])
            .context("Unable to assemble inline playback video sink")?;
        gst::Element::link_many(&[
            &queue,
            &deinterlace,
            &convert,
            &scale,
            &capsfilter,
            &sink_element,
        ])
        .context("Unable to link inline playback video sink")?;

        let sink_pad = queue
            .static_pad("sink")
            .ok_or_else(|| anyhow!("queue element is missing a sink pad"))?;
        let ghost_pad = gst::GhostPad::builder_with_target(&sink_pad)
            .map_err(|err| anyhow!("Unable to configure inline playback ghost pad builder: {err}"))?
            .name("sink")
            .build();
        ghost_pad
            .set_active(true)
            .context("Unable to activate inline playback ghost pad")?;
        video_bin
            .add_pad(&ghost_pad)
            .context("Unable to expose inline playback sink to GStreamer playbin")?;

        let pipeline = gst::ElementFactory::make("playbin")
            .property("uri", uri)
            .property("video-sink", &video_bin)
            .build()
            .map_err(|err| anyhow!("GStreamer 'playbin' element is not available: {err}"))?;

        let caps_for_convert = video_caps.clone();
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let mut sample = match sink.pull_sample() {
                        Ok(sample) => sample,
                        Err(err) => {
                            warn!("Failed to pull sample: {err}");
                            return Err(gst::FlowError::Error);
                        }
                    };

                    let mut caps = sample.caps().ok_or(gst::FlowError::Error)?;
                    let mut info = VideoInfo::from_caps(caps).map_err(|_| gst::FlowError::Error)?;
                    let mut converted_from: Option<VideoFormat> = None;

                    if info.format() != VideoFormat::Rgba {
                        let original_format = info.format();
                        match gstreamer_video::convert_sample(
                            &sample,
                            &caps_for_convert,
                            gst::ClockTime::MAX,
                        ) {
                            Ok(converted) => {
                                sample = converted;
                                caps = sample.caps().ok_or(gst::FlowError::Error)?;
                                info = VideoInfo::from_caps(caps)
                                    .map_err(|_| gst::FlowError::Error)?;
                                converted_from = Some(original_format);
                            }
                            Err(err) => {
                                warn!("Unable to convert sample to RGBA: {err}");
                                return Err(gst::FlowError::Error);
                            }
                        }
                    }

                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    if buffer.flags().contains(gst::BufferFlags::GAP) {
                        let mut state = shared.lock().expect("player state poisoned");
                        state.blank_frames = state.blank_frames.saturating_add(1);
                        let deadline_expired = state
                            .first_frame_deadline
                            .map(|deadline| Instant::now() > deadline)
                            .unwrap_or(false);
                        if !state.ever_had_frame {
                            if deadline_expired {
                                warn!(
                                    "Inline playback timed out waiting for first decoded frame after {:?}",
                                    FIRST_FRAME_TIMEOUT
                                );
                                state.needs_restart = true;
                            } else if state.blank_frames == BLANK_FRAME_THRESHOLD {
                                warn!("Inline playback received {BLANK_FRAME_THRESHOLD} consecutive GAP buffers before the first decoded frame");
                            }
                        } else if state.blank_frames == BLANK_FRAME_THRESHOLD {
                            warn!("Inline playback received {BLANK_FRAME_THRESHOLD} consecutive GAP buffers");
                        }
                        trace!("Skipping GAP buffer while waiting for decoded video frame");
                        return Ok(gst::FlowSuccess::Ok);
                    }
                    let frame = VideoFrameRef::from_buffer_ref_readable(buffer, &info)
                        .map_err(|err| {
                            warn!("Unable to map video frame: {err}");
                            gst::FlowError::Error
                        })?;

                    let width = info.width() as usize;
                    let height = info.height() as usize;
                    if width == 0 || height == 0 {
                        warn!("Received zero-sized frame");
                        return Err(gst::FlowError::Error);
                    }

                    let row_bytes = width.saturating_mul(4);
                    let mut pixels = vec![0_u8; width * height * 4];
                    let data = frame
                        .plane_data(0)
                        .map_err(|err| {
                            warn!("Inline playback frame missing RGBA plane 0: {err}");
                            gst::FlowError::Error
                        })?;
                    let stride_i32 = frame
                        .plane_stride()
                        .first()
                        .copied()
                        .ok_or_else(|| {
                            warn!("Inline playback frame missing stride information");
                            gst::FlowError::Error
                        })?;
                    let stride = usize::try_from(stride_i32).map_err(|_| {
                        warn!(stride = stride_i32, "Inline playback frame reported negative stride");
                        gst::FlowError::Error
                    })?;
                    if data.len() < stride.saturating_mul(height) {
                        warn!(
                            data_len = data.len(),
                            stride,
                            height,
                            "Plane data shorter than expected"
                        );
                        return Err(gst::FlowError::Error);
                    }
                    if stride == row_bytes {
                        pixels.copy_from_slice(&data[..row_bytes * height]);
                    } else {
                        for (y, dst_row) in pixels.chunks_mut(row_bytes).enumerate() {
                            let start = y.saturating_mul(stride);
                            let end = start + row_bytes;
                            if end > data.len() {
                                warn!(
                                    row = y,
                                    data_len = data.len(),
                                    required = end,
                                    "Inline playback RGBA row exceeds plane data"
                                );
                                return Err(gst::FlowError::Error);
                            }
                            dst_row.copy_from_slice(&data[start..end]);
                        }
                    }
                    let stride_bytes = row_bytes;

                    let debug_index = FRAME_DEBUG_LOG_COUNT.fetch_add(1, Ordering::Relaxed);
                    let mut min_r = u8::MAX;
                    let mut max_r = u8::MIN;
                    let mut min_g = u8::MAX;
                    let mut max_g = u8::MIN;
                    let mut min_b = u8::MAX;
                    let mut max_b = u8::MIN;
                    for chunk in pixels.chunks_exact(4) {
                        min_r = min_r.min(chunk[0]);
                        max_r = max_r.max(chunk[0]);
                        min_g = min_g.min(chunk[1]);
                        max_g = max_g.max(chunk[1]);
                        min_b = min_b.min(chunk[2]);
                        max_b = max_b.max(chunk[2]);
                    }
                    if debug_index < MAX_DEBUG_FRAMES {
                        debug!(
                            format = ?info.format(),
                            converted_from = ?converted_from,
                            stride = stride_bytes,
                            plane_len = pixels.len(),
                            width,
                            height,
                            sample = %format_sample_rgba_debug(&pixels),
                            min_r,
                            max_r,
                            min_g,
                            max_g,
                            min_b,
                            max_b,
                            "Inline playback frame (debug sample)"
                        );
                    } else {
                        trace!(format = ?info.format(), "Inline playback frame");
                    }

                    if max_r == 0 && max_g == 0 && max_b == 0 {
                        let mut state = shared.lock().expect("player state poisoned");
                        state.blank_frames = state.blank_frames.saturating_add(1);
                        let deadline_expired = state
                            .first_frame_deadline
                            .map(|deadline| Instant::now() > deadline)
                            .unwrap_or(false);
                        if !state.ever_had_frame {
                            if deadline_expired {
                                warn!(
                                    "Inline playback timed out waiting for first decoded frame after {:?}",
                                    FIRST_FRAME_TIMEOUT
                                );
                                state.needs_restart = true;
                            } else if state.blank_frames == BLANK_FRAME_THRESHOLD {
                                warn!("Inline playback received {BLANK_FRAME_THRESHOLD} consecutive blank frames before the first decoded frame");
                            }
                        } else if state.blank_frames == BLANK_FRAME_THRESHOLD {
                            warn!("Inline playback received {BLANK_FRAME_THRESHOLD} consecutive blank frames");
                        }
                        trace!("Skipping fully black frame to avoid flashing");
                        return Ok(gst::FlowSuccess::Ok);
                    }

                    // Ensure fully opaque output since streamed sources rarely carry alpha, and
                    // an unexpected zero alpha channel results in an invisible or magenta frame.
                    for px in pixels.chunks_exact_mut(4) {
                        px[3] = 0xFF;
                    }

                    let mut state = shared.lock().expect("player state poisoned");
                    state.blank_frames = 0;
                    state.last_error = None;
                    state.ever_had_frame = true;
                    state.first_frame_deadline = None;
                    state.frame_version = state.frame_version.wrapping_add(1);
                    state.frame = Some(DecodedFrame {
                        width,
                        height,
                        pixels,
                        version: state.frame_version,
                    });
                    state.last_error = None;
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        pipeline
            .set_state(gst::State::Ready)
            .context("Unable to prepare pipeline for playback")?;

        pipeline
            .set_state(gst::State::Playing)
            .context("Unable to start playback")?;

        let stop_flag = Arc::new(AtomicBool::new(false));
        let bus = pipeline
            .bus()
            .ok_or_else(|| anyhow!("Pipeline did not provide a bus"))?;
        let shared_for_bus = Arc::clone(&self.shared);
        let stop_for_bus = Arc::clone(&stop_flag);

        let timeout = gst::ClockTime::from_mseconds(BUS_POLL_TIMEOUT_MS);
        let thread_handle = thread::spawn(move || {
            while !stop_for_bus.load(Ordering::Relaxed) {
                match bus.timed_pop(timeout) {
                    Some(message) => match message.view() {
                        gst::message::MessageView::Error(err) => {
                            let mut state = shared_for_bus.lock().expect("player state poisoned");
                            state.last_error = Some(format!("Playback error: {}", err.error()));
                            break;
                        }
                        gst::message::MessageView::Eos(_) => {
                            debug!("Reached end of stream");
                            break;
                        }
                        _ => {}
                    },
                    None => continue,
                }
            }
        });

        self.current_uri = Some(uri.to_owned());
        self.appsink = Some(appsink);
        self.pipeline = Some(pipeline);
        self.bus_thread = Some(thread_handle);
        self.stop_flag = Some(stop_flag);
        FRAME_DEBUG_LOG_COUNT.store(0, Ordering::Relaxed);

        Ok(())
    }

    pub fn poll_restart(&mut self) {
        let should_restart = {
            let mut state = self.shared.lock().expect("player state poisoned");
            if state.needs_restart {
                state.needs_restart = false;
                true
            } else {
                false
            }
        };

        if !should_restart {
            return;
        }

        if self.restart_attempts >= MAX_STALL_RESTARTS {
            let mut state = self.shared.lock().expect("player state poisoned");
            if state.last_error.is_none() {
                state.last_error = Some(
                    "Stream is not producing any video frames. Try opening externally.".to_owned(),
                );
            }
            return;
        }

        let allow_restart = self
            .last_restart
            .map(|last| last.elapsed() >= STALL_RESTART_BACKOFF)
            .unwrap_or(true);

        if !allow_restart {
            let mut state = self.shared.lock().expect("player state poisoned");
            state.needs_restart = true;
            return;
        }

        self.restart_attempts += 1;
        self.last_restart = Some(Instant::now());
        warn!(
            "Inline playback restarting stream after blank-frame stall (attempt {})",
            self.restart_attempts
        );

        if let Err(err) = self.restart() {
            let mut state = self.shared.lock().expect("player state poisoned");
            state.last_error = Some(format!("Unable to restart playback: {err}"));
        }
    }

    pub fn stop(&mut self) {
        if let Some(flag) = &self.stop_flag {
            flag.store(true, Ordering::Relaxed);
        }

        if let Some(pipeline) = self.pipeline.take() {
            let _ = pipeline.set_state(gst::State::Ready);
            let _ = pipeline.set_state(gst::State::Null);
        }

        if let Some(handle) = self.bus_thread.take() {
            let _ = handle.join();
        }

        self.appsink = None;
        self.stop_flag = None;
        self.current_uri = None;

        if let Ok(mut state) = self.shared.lock() {
            state.blank_frames = 0;
            state.needs_restart = false;
            state.ever_had_frame = false;
            state.frame = None;
            state.frame_version = 0;
            state.first_frame_deadline = None;
        }
    }

    pub fn latest_frame(&self) -> Option<DecodedFrame> {
        let state = self.shared.lock().expect("player state poisoned");
        state.frame.clone()
    }

    pub fn take_error(&mut self) -> Option<String> {
        let mut state = self.shared.lock().expect("player state poisoned");
        state.last_error.take()
    }
}

#[cfg(feature = "inline-playback")]
fn format_sample_rgba_debug(data: &[u8]) -> String {
    let sample_len = data.len().min(16);
    data[..sample_len]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

impl Drop for LivestreamPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(all(test, feature = "inline-playback"))]
mod tests {}
