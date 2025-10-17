//! GStreamer-backed video playback manager for Notedeck.
//!
//! This module brokers communication between the egui UI thread and a
//! background GStreamer worker. The UI requests players for HTTP(S) MP4 URLs,
//! and the backend handles progressive download, hardware-accelerated decode,
//! and surface extraction into CPU RGBA frames suitable for upload to wgpu.

use std::cmp::min;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, RecvTimeoutError, Sender};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use once_cell::sync::Lazy;
use tracing::{debug, error, trace, warn};
use url::Url;

static GST_INIT: Lazy<Result<(), gst::glib::Error>> = Lazy::new(gst::init);

/// Unique identifier for a video session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VideoId(u64);

impl VideoId {
    fn next(counter: &mut u64) -> Self {
        let id = *counter;
        *counter = counter.wrapping_add(1);
        VideoId(id)
    }

    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

/// RGBA frame extracted from the GStreamer pipeline.
#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<[u8]>,
    pub generation: u64,
    pub timestamp: Option<Duration>,
}

#[derive(Debug, Clone)]
pub struct VideoState {
    pub id: VideoId,
    pub url: Url,
    pub created_at: Instant,
    pub status: VideoStatus,
    pub poster: Option<Arc<VideoFrame>>,
    pub current_frame: Option<Arc<VideoFrame>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoStatus {
    Idle,
    Opening,
    Paused,
    Playing,
    Ended,
    Failed(String),
}

#[derive(Debug, Clone)]
pub enum VideoEvent {
    StateChanged(VideoState),
    PosterReady { id: VideoId, frame: Arc<VideoFrame> },
    FrameReady { id: VideoId, frame: Arc<VideoFrame> },
}

#[derive(Debug, Clone)]
pub struct VideoManagerConfig {
    pub user_agent: String,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
}

impl Default for VideoManagerConfig {
    fn default() -> Self {
        Self {
            user_agent: "Notedeck/0.0 (video)".to_string(),
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(20),
        }
    }
}

pub struct VideoManager {
    config: VideoManagerConfig,
    enabled: bool,
    counter: u64,
    players: HashMap<VideoId, VideoState>,
    url_index: HashMap<String, VideoId>,
    events: Vec<VideoEvent>,
    backend: Option<BackendHandle>,
}

impl Default for VideoManager {
    fn default() -> Self {
        Self::new(VideoManagerConfig::default())
    }
}

impl VideoManager {
    pub fn new(config: VideoManagerConfig) -> Self {
        let enabled = match &*GST_INIT {
            Ok(_) => true,
            Err(err) => {
                error!("Failed to initialize GStreamer: {err}");
                false
            }
        };

        let backend = if enabled {
            BackendHandle::spawn()
        } else {
            None
        };

        Self {
            config,
            enabled,
            counter: 0,
            players: HashMap::new(),
            url_index: HashMap::new(),
            events: Vec::new(),
            backend,
        }
    }

    pub fn ensure_player_from_str(&mut self, url: &str) -> Result<VideoHandle, url::ParseError> {
        let parsed = Url::parse(url)?;
        Ok(self.ensure_player(parsed))
    }

    pub fn ensure_player(&mut self, url: Url) -> VideoHandle {
        if let Some(&id) = self.url_index.get(url.as_str()) {
            return VideoHandle { id };
        }

        let id = VideoId::next(&mut self.counter);

        let state = VideoState {
            id,
            url: url.clone(),
            created_at: Instant::now(),
            status: VideoStatus::Opening,
            poster: None,
            current_frame: None,
        };

        self.players.insert(id, state.clone());
        self.url_index.insert(url.as_str().to_owned(), id);

        self.events.push(VideoEvent::StateChanged(state));

        if let Some(backend) = &self.backend {
            backend.send(BackendCommand::Create {
                id,
                url: url.to_string(),
                config: self.config.clone(),
            });
        } else {
            self.fail_player(id, "Video backend unavailable".to_string());
        }

        VideoHandle { id }
    }

    pub fn create_player(&mut self, url: Url) -> VideoHandle {
        self.ensure_player(url)
    }

    pub fn handle_for_str(&self, url: &str) -> Option<VideoHandle> {
        Url::parse(url)
            .ok()
            .and_then(|parsed| self.handle_for_url(&parsed))
    }

    pub fn handle_for_url(&self, url: &Url) -> Option<VideoHandle> {
        self.url_index
            .get(url.as_str())
            .copied()
            .map(|id| VideoHandle { id })
    }

    pub fn play(&mut self, handle: VideoHandle) {
        self.pump_backend_events();
        if let Some(state) = self.players.get_mut(&handle.id) {
            if !matches!(state.status, VideoStatus::Playing) {
                state.status = VideoStatus::Playing;
                self.events.push(VideoEvent::StateChanged(state.clone()));
            }
        }
        if let Some(backend) = &self.backend {
            backend.send(BackendCommand::Play { id: handle.id });
        }
    }

    pub fn pause(&mut self, handle: VideoHandle) {
        self.pump_backend_events();
        if let Some(state) = self.players.get_mut(&handle.id) {
            if !matches!(state.status, VideoStatus::Paused) {
                state.status = VideoStatus::Paused;
                self.events.push(VideoEvent::StateChanged(state.clone()));
            }
        }
        if let Some(backend) = &self.backend {
            backend.send(BackendCommand::Pause { id: handle.id });
        }
    }

    pub fn drop_player(&mut self, handle: VideoHandle) {
        self.pump_backend_events();

        if let Some(state) = self.players.remove(&handle.id) {
            self.events.push(VideoEvent::StateChanged(VideoState {
                status: VideoStatus::Ended,
                ..state
            }));
        }

        self.url_index.retain(|_, id| *id != handle.id);

        if let Some(backend) = &self.backend {
            backend.send(BackendCommand::Drop { id: handle.id });
        }
    }

    pub fn drain_events(&mut self) -> impl Iterator<Item = VideoEvent> + '_ {
        self.pump_backend_events();
        self.events.drain(..)
    }

    pub fn state(&mut self, handle: VideoHandle) -> Option<VideoState> {
        self.pump_backend_events();
        self.players.get(&handle.id).cloned()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled && self.backend.is_some()
    }

    fn pump_backend_events(&mut self) {
        let events: Vec<_> = match &self.backend {
            Some(backend) => backend.events.try_iter().collect(),
            None => return,
        };

        for event in events {
            match event {
                BackendEvent::Poster { id, frame } => {
                    if let Some(state) = self.players.get_mut(&id) {
                        state.poster = Some(frame.clone());
                        if state.current_frame.is_none() {
                            state.current_frame = Some(frame.clone());
                        }
                        if matches!(state.status, VideoStatus::Opening | VideoStatus::Idle) {
                            state.status = VideoStatus::Paused;
                        }
                        self.events.push(VideoEvent::PosterReady { id, frame });
                        self.events.push(VideoEvent::StateChanged(state.clone()));
                    }
                }
                BackendEvent::Frame { id, frame } => {
                    if let Some(state) = self.players.get_mut(&id) {
                        state.current_frame = Some(frame.clone());
                        self.events.push(VideoEvent::FrameReady { id, frame });
                    }
                }
                BackendEvent::State { id, status } => {
                    if let Some(state) = self.players.get_mut(&id) {
                        state.status = status.clone();
                        if matches!(status, VideoStatus::Ended | VideoStatus::Failed(_)) {
                            // Keep last frame/poster for UI overlays.
                        }
                        self.events.push(VideoEvent::StateChanged(state.clone()));
                    }
                }
                BackendEvent::Error { id, message } => {
                    error!(
                        video_id = id.as_u64(),
                        %message,
                        "video backend reported error"
                    );
                    self.fail_player(id, message);
                }
            }
        }
    }

    fn fail_player(&mut self, id: VideoId, message: String) {
        if let Some(state) = self.players.get_mut(&id) {
            state.status = VideoStatus::Failed(message.clone());
            self.events.push(VideoEvent::StateChanged(state.clone()));
        }
    }
}

impl Drop for VideoManager {
    fn drop(&mut self) {
        if let Some(mut backend) = self.backend.take() {
            backend.send(BackendCommand::Shutdown);
            if let Some(handle) = backend.join_handle.take() {
                if let Err(err) = handle.join() {
                    warn!("Failed to join video backend thread: {err:?}");
                }
            }
        }
        self.players.clear();
        self.url_index.clear();
        self.events.clear();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VideoHandle {
    pub id: VideoId,
}

struct BackendHandle {
    cmd_tx: Sender<BackendCommand>,
    events: Receiver<BackendEvent>,
    join_handle: Option<thread::JoinHandle<()>>,
}

impl BackendHandle {
    fn spawn() -> Option<Self> {
        let (cmd_tx, cmd_rx) = unbounded();
        let (evt_tx, evt_rx) = unbounded();

        let handle = thread::Builder::new()
            .name("notedeck-gst-video".into())
            .spawn(move || backend_main(cmd_rx, evt_tx))
            .map_err(|err| {
                error!("Failed to spawn video backend thread: {err}");
            })
            .ok()?;

        Some(Self {
            cmd_tx,
            events: evt_rx,
            join_handle: Some(handle),
        })
    }

    fn send(&self, command: BackendCommand) {
        if let Err(err) = self.cmd_tx.send(command) {
            debug!("Video backend channel closed: {err}");
        }
    }
}

#[derive(Clone)]
struct BackendPlayer {
    id: VideoId,
    playbin: gst::Element,
    bus: gst::Bus,
    _appsink: gst_app::AppSink,
}

enum BackendCommand {
    Create {
        id: VideoId,
        url: String,
        config: VideoManagerConfig,
    },
    Play {
        id: VideoId,
    },
    Pause {
        id: VideoId,
    },
    Drop {
        id: VideoId,
    },
    Shutdown,
}

enum BackendEvent {
    Poster { id: VideoId, frame: Arc<VideoFrame> },
    Frame { id: VideoId, frame: Arc<VideoFrame> },
    State { id: VideoId, status: VideoStatus },
    Error { id: VideoId, message: String },
}

fn backend_main(cmd_rx: Receiver<BackendCommand>, evt_tx: Sender<BackendEvent>) {
    let mut players: HashMap<VideoId, BackendPlayer> = HashMap::new();

    loop {
        // Periodically drain bus messages even if no new commands arrive.
        pump_buses(&players, &evt_tx);

        match cmd_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(BackendCommand::Create { id, url, config }) => {
                if players.contains_key(&id) {
                    continue;
                }

                match BackendPlayer::new(id, url.clone(), config.clone(), evt_tx.clone()) {
                    Ok(player) => {
                        let playbin = player.playbin.clone();
                        match playbin.set_state(gst::State::Paused) {
                            Ok(result) => {
                                trace!(id = id.as_u64(), ?result, "Initialized playbin to paused");
                            }
                            Err(err) => {
                                let message = format!("Failed to pre-roll video pipeline: {err:?}");
                                let _ = evt_tx.send(BackendEvent::Error { id, message });
                            }
                        }
                        players.insert(id, player.clone());
                    }
                    Err(err) => {
                        let message = format!("Failed to create player: {err}");
                        let _ = evt_tx.send(BackendEvent::Error { id, message });
                    }
                }
            }
            Ok(BackendCommand::Play { id }) => {
                if let Some(player) = players.get(&id) {
                    set_pipeline_state(&player.playbin, gst::State::Playing, &evt_tx, id);
                }
            }
            Ok(BackendCommand::Pause { id }) => {
                if let Some(player) = players.get(&id) {
                    set_pipeline_state(&player.playbin, gst::State::Paused, &evt_tx, id);
                }
            }
            Ok(BackendCommand::Drop { id }) => {
                if let Some(player) = players.remove(&id) {
                    let _ = player.playbin.set_state(gst::State::Null);
                    let _ = evt_tx.send(BackendEvent::State {
                        id,
                        status: VideoStatus::Ended,
                    });
                }
            }
            Ok(BackendCommand::Shutdown) => break,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    // Clean shutdown of players.
    for (_, player) in players {
        let _ = player.playbin.set_state(gst::State::Null);
    }
}

fn pump_buses(players: &HashMap<VideoId, BackendPlayer>, evt_tx: &Sender<BackendEvent>) {
    for player in players.values() {
        while let Some(msg) = player.bus.pop() {
            handle_bus_message(player, msg, evt_tx);
        }
    }
}

fn handle_bus_message(player: &BackendPlayer, msg: gst::Message, evt_tx: &Sender<BackendEvent>) {
    use gst::MessageView;

    match msg.view() {
        MessageView::Error(err) => {
            let mut debug_msg = err.debug().map(|d| d.to_string()).unwrap_or_default();
            if debug_msg.len() > 512 {
                debug_msg.truncate(512);
                debug_msg.push('…');
            }
            let message = if debug_msg.is_empty() {
                err.error().to_string()
            } else {
                format!("{} ({debug_msg})", err.error())
            };
            let _ = evt_tx.send(BackendEvent::Error {
                id: player.id,
                message,
            });
        }
        MessageView::Eos(_) => {
            let _ = evt_tx.send(BackendEvent::State {
                id: player.id,
                status: VideoStatus::Ended,
            });
        }
        MessageView::StateChanged(state) => {
            let Some(src) = state.src() else {
                return;
            };

            if src.as_ptr() != player.playbin.as_ptr() as *mut gst::ffi::GstObject {
                return;
            }

            let status = match state.current() {
                gst::State::Playing => VideoStatus::Playing,
                gst::State::Paused | gst::State::Ready => VideoStatus::Paused,
                gst::State::Null => VideoStatus::Ended,
                _ => VideoStatus::Opening,
            };

            let _ = evt_tx.send(BackendEvent::State {
                id: player.id,
                status,
            });
        }
        _ => {}
    }
}

fn set_pipeline_state(
    playbin: &gst::Element,
    state: gst::State,
    evt_tx: &Sender<BackendEvent>,
    id: VideoId,
) {
    match playbin.set_state(state) {
        Ok(gst::StateChangeSuccess::Async) | Ok(gst::StateChangeSuccess::Success) => {}
        Ok(gst::StateChangeSuccess::NoPreroll) => {
            trace!(id = id.as_u64(), ?state, "pipeline reported NoPreroll");
        }
        Err(err) => {
            let _ = evt_tx.send(BackendEvent::Error {
                id,
                message: format!("Failed to change state to {state:?}: {err:?}"),
            });
        }
    }
}

impl BackendPlayer {
    fn new(
        id: VideoId,
        url: String,
        config: VideoManagerConfig,
        evt_tx: Sender<BackendEvent>,
    ) -> Result<Self, String> {
        let playbin = gst::ElementFactory::make("playbin")
            .build()
            .map_err(|_| "Missing GStreamer element 'playbin'".to_string())?;

        playbin.set_property("uri", url.as_str());

        if playbin.has_property("user-agent", None) {
            playbin.set_property("user-agent", config.user_agent.as_str());
        } else {
            trace!("playbin missing user-agent property; skipping set");
        }

        let generation = Arc::new(AtomicU64::new(1));
        let has_poster = Arc::new(AtomicBool::new(false));

        let appsink =
            Self::configure_video_sink(id, evt_tx.clone(), generation.clone(), has_poster.clone())?;

        playbin.set_property("video-sink", &appsink);

        Self::configure_source(&playbin, &config);

        let bus = playbin
            .bus()
            .ok_or_else(|| "playbin missing bus".to_string())?;

        Ok(Self {
            id,
            playbin,
            bus,
            _appsink: appsink,
        })
    }

    fn configure_video_sink(
        id: VideoId,
        evt_tx: Sender<BackendEvent>,
        generation: Arc<AtomicU64>,
        has_poster: Arc<AtomicBool>,
    ) -> Result<gst_app::AppSink, String> {
        let element = gst::ElementFactory::make("appsink")
            .property("sync", false)
            .build()
            .map_err(|_| "Missing GStreamer element 'appsink'".to_string())?;

        let appsink = element
            .downcast::<gst_app::AppSink>()
            .map_err(|_| "Failed to downcast appsink".to_string())?;

        let caps = gst::Caps::builder("video/x-raw")
            .field("format", &"RGBA")
            .field("pixel-aspect-ratio", &gst::Fraction::new(1, 1))
            .build();
        appsink.set_caps(Some(&caps));
        appsink.set_drop(true);
        appsink.set_max_buffers(4);
        appsink.set_qos(true);

        let video_id = id;
        let callbacks = gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| match sink.pull_sample() {
                Ok(sample) => {
                    if let Err(err) = convert_sample_to_frame(
                        &sample,
                        video_id,
                        &generation,
                        &has_poster,
                        &evt_tx,
                    ) {
                        let _ = evt_tx.send(BackendEvent::Error {
                            id: video_id,
                            message: err,
                        });
                        return Err(gst::FlowError::Error);
                    }
                    Ok(gst::FlowSuccess::Ok)
                }
                Err(err) => {
                    trace!(
                        id = video_id.as_u64(),
                        error = %err,
                        "appsink pull_sample returned BoolError; treating as flushing"
                    );
                    Err(gst::FlowError::Flushing)
                }
            })
            .build();

        appsink.set_callbacks(callbacks);

        Ok(appsink)
    }

    fn configure_source(playbin: &gst::Element, config: &VideoManagerConfig) {
        let connect_timeout = seconds_clamped(config.connect_timeout);
        let read_timeout = seconds_clamped(config.read_timeout);
        let user_agent = config.user_agent.clone();

        let _ = playbin.connect("source-setup", false, move |values| {
            if let Some(value) = values.get(1) {
                if let Ok(source_obj) = value.get::<gst::Object>() {
                    if let Ok(element) = source_obj.downcast::<gst::Element>() {
                        if element.has_property("user-agent", None) {
                            element.set_property("user-agent", user_agent.as_str());
                        }
                        if element.has_property("timeout", None) {
                            element.set_property("timeout", connect_timeout);
                        }
                        if element.has_property("read-timeout", None) {
                            element.set_property("read-timeout", read_timeout);
                        }
                        if element.has_property("connect-timeout", None) {
                            element.set_property("connect-timeout", connect_timeout);
                        }
                    }
                }
            }
            None
        });
    }
}

fn seconds_clamped(duration: Duration) -> u32 {
    min(duration.as_secs(), u32::MAX as u64) as u32
}

fn convert_sample_to_frame(
    sample: &gst::Sample,
    id: VideoId,
    generation: &Arc<AtomicU64>,
    has_poster: &Arc<AtomicBool>,
    evt_tx: &Sender<BackendEvent>,
) -> Result<(), String> {
    let caps = sample
        .caps()
        .ok_or_else(|| "sample missing caps".to_string())?;

    let info = gst_video::VideoInfo::from_caps(caps)
        .map_err(|err| format!("failed to parse video info: {err}"))?;

    let buffer = sample
        .buffer()
        .ok_or_else(|| "sample missing buffer".to_string())?;

    let map = buffer
        .map_readable()
        .map_err(|_| "failed to map buffer".to_string())?;

    let data = map.as_slice();

    let width = info.width();
    let height = info.height();

    let stride = info.stride()[0] as usize;
    let row_bytes = (width as usize) * 4;

    let mut pixels = vec![0u8; row_bytes * (height as usize)];

    let pixels_len = pixels.len();

    if stride == row_bytes && data.len() >= pixels_len {
        pixels.copy_from_slice(&data[..pixels_len]);
    } else {
        let available = data.len();
        for y in 0..height as usize {
            let src_offset = y * stride;
            let dst_offset = y * row_bytes;
            if dst_offset + row_bytes > pixels_len {
                break;
            }
            if src_offset + row_bytes > available {
                break;
            }
            pixels[dst_offset..dst_offset + row_bytes]
                .copy_from_slice(&data[src_offset..src_offset + row_bytes]);
        }
    }

    let generation_value = generation.fetch_add(1, Ordering::SeqCst);

    let timestamp = buffer
        .pts()
        .or_else(|| buffer.dts())
        .map(|clock_time| Duration::from_nanos(clock_time.nseconds().into()));

    let frame = Arc::new(VideoFrame {
        width,
        height,
        pixels: pixels.into(),
        generation: generation_value,
        timestamp,
    });

    trace!(
        video_id = id.as_u64(),
        generation = generation_value,
        width,
        height,
        pts_ns = frame
            .timestamp
            .map(|ts| ts.as_nanos() as u64)
            .unwrap_or_default(),
        "converted video frame"
    );

    if !has_poster.swap(true, Ordering::SeqCst) {
        let _ = evt_tx.send(BackendEvent::Poster {
            id,
            frame: frame.clone(),
        });
    }

    let _ = evt_tx.send(BackendEvent::Frame { id, frame });

    Ok(())
}
