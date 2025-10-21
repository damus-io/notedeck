//! Android backend for egui-video – backed by MediaCodec + ImageReader.

use std::{
    ffi::CString,
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use anyhow::{anyhow, Result};
use egui::{
    vec2, Color32, ColorImage, FontId, Image, Rect, Response, Sense, TextureHandle, TextureOptions,
    Ui, Vec2,
};
use ndk::{
    hardware_buffer::HardwareBufferUsage,
    media::{
        image_reader::{AcquireResult, ImageReader},
        media_codec::{
            DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodec,
            MediaCodecDirection,
        },
        media_format::MediaFormat,
    },
    media_error::MediaError,
};
use ndk_sys as ffi;
use ndk_sys::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM;

/// Placeholder audio device – audio is not yet supported on Android.
pub type AudioDevice = ();

/// Ensure global state is initialised. Nothing required for the Android backend yet.
pub fn ensure_initialized() {}

/// Initialise audio playback – Android backend does not expose audio yet.
pub fn init_audio_device<T>(_audio_sys: &T) -> Result<AudioDevice, String> {
    Ok(())
}

/// Cache wrapper mirroring the desktop backend API.
#[derive(Clone)]
pub struct Cache<T: Copy> {
    cached_value: T,
    override_value: Option<T>,
    raw_value: Arc<Mutex<T>>,
}

impl<T: Copy> Cache<T> {
    pub fn new(value: T) -> Self {
        Self {
            cached_value: value,
            override_value: None,
            raw_value: Arc::new(Mutex::new(value)),
        }
    }

    pub fn set(&mut self, value: T) {
        self.cached_value = value;
        if let Ok(mut guard) = self.raw_value.lock() {
            *guard = value;
        }
    }

    pub fn get(&mut self) -> T {
        if let Some(value) = self.override_value {
            return value;
        }
        self.try_update_cache();
        self.cached_value
    }

    pub fn get_true(&mut self) -> T {
        self.try_update_cache();
        self.cached_value
    }

    pub fn try_update_cache(&mut self) -> Option<T> {
        if let Ok(mut guard) = self.raw_value.try_lock() {
            self.cached_value = *guard;
            Some(self.cached_value)
        } else {
            None
        }
    }
}

/// Playback state, kept in sync with the desktop backend.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlayerState {
    Stopped,
    EndOfFile,
    Seeking(f32),
    Paused,
    Playing,
}

impl Default for PlayerState {
    fn default() -> Self {
        PlayerState::Stopped
    }
}

/// Android-backed video player.
pub struct Player {
    pub player_state: Cache<PlayerState>,
    pub texture_handle: TextureHandle,
    pub height: u32,
    pub width: u32,
    pub looping: bool,
    pub audio_volume: Cache<f32>,
    pub max_audio_volume: f32,

    texture_options: TextureOptions,
    ctx: egui::Context,
    duration_ms: i64,
    video_elapsed_ms: Cache<i64>,
    pending_frames: Arc<Mutex<Option<FrameData>>>,
    command_tx: Sender<DecoderCommand>,
    stop_flag: Arc<AtomicBool>,
    join_handle: Option<thread::JoinHandle<()>>,
    error_slot: Arc<Mutex<Option<String>>>,
}

struct FrameData {
    pixels: Vec<u8>,
    width: usize,
    height: usize,
    timestamp_ms: i64,
}

enum DecoderCommand {
    Play,
    Pause,
    Stop,
    Quit,
    SeekFraction(f32),
}

#[derive(Clone)]
struct DecoderInit {
    width: u32,
    height: u32,
    duration_ms: i64,
    frame_rate: f32,
}

impl Player {
    /// Create a player for the provided video path (local file).
    pub fn new(ctx: &egui::Context, input_path: &String) -> Result<Self> {
        let texture_options = TextureOptions::LINEAR;
        let texture_handle = ctx.load_texture(
            "android-video-placeholder",
            ColorImage::example(),
            texture_options,
        );

        let player_state = Cache::new(PlayerState::Stopped);
        let video_elapsed_ms = Cache::new(0);
        let audio_volume = Cache::new(0.5);

        let frames = Arc::new(Mutex::new(None));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let error_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let (command_tx, command_rx) = mpsc::channel::<DecoderCommand>();
        let (init_tx, init_rx) = mpsc::channel::<Result<DecoderInit, String>>();

        let decoder_input_path = input_path.clone();
        let state_for_thread = player_state.clone();
        let elapsed_for_thread = video_elapsed_ms.clone();
        let frames_for_thread = Arc::clone(&frames);
        let stop_for_thread = Arc::clone(&stop_flag);
        let error_for_thread = Arc::clone(&error_slot);

        let join_handle = thread::Builder::new()
            .name("android-video-decode".to_string())
            .spawn(move || {
                let init_result = DecoderThread::spawn(
                    decoder_input_path,
                    state_for_thread,
                    elapsed_for_thread,
                    frames_for_thread,
                    command_rx,
                    stop_for_thread,
                );

                match init_result {
                    Ok(mut decoder) => {
                        if let Err(e) = init_tx.send(Ok(decoder.init.clone())) {
                            let _ = e;
                        }
                        if let Err(err) = decoder.run() {
                            if let Ok(mut slot) = error_for_thread.lock() {
                                *slot = Some(err.to_string());
                            }
                        }
                    }
                    Err(err) => {
                        let _ = init_tx.send(Err(err.to_string()));
                    }
                }
            })
            .map_err(|e| anyhow!("failed to spawn decoder thread: {e}"))?;

        let init = init_rx
            .recv_timeout(Duration::from_secs(3))
            .map_err(|_| anyhow!("timeout initialising video decoder"))?
            .map_err(|err| anyhow!("{err}"))?;

        Ok(Self {
            player_state,
            texture_handle,
            height: init.height,
            width: init.width,
            looping: false,
            audio_volume,
            max_audio_volume: 1.0,
            texture_options,
            ctx: ctx.clone(),
            duration_ms: init.duration_ms,
            video_elapsed_ms,
            pending_frames: frames,
            command_tx,
            stop_flag,
            join_handle: Some(join_handle),
            error_slot,
        })
    }

    /// Tie audio device to the player – currently a no-op on Android.
    pub fn with_audio(self, _audio_device: &mut AudioDevice) -> Result<Self> {
        Ok(self)
    }

    /// Draw the player UI inside the given bounds.
    pub fn ui(&mut self, ui: &mut Ui, size: [f32; 2]) -> Response {
        self.drain_new_frame();
        self.process_errors(ui);

        let image = Image::new((self.texture_handle.id(), Vec2::new(size[0], size[1])))
            .sense(Sense::click());
        let response = ui.add(image);
        self.render_overlay(ui, &response);
        response
    }

    /// Draw the player UI inside an explicit rectangle.
    pub fn ui_at(&mut self, ui: &mut Ui, rect: Rect) -> Response {
        self.drain_new_frame();
        self.process_errors(ui);

        let image =
            Image::new((self.texture_handle.id(), rect.size().clone())).sense(Sense::click());
        let response = ui.put(rect, image);
        self.render_overlay(ui, &response);
        response
    }

    fn render_overlay(&mut self, ui: &mut Ui, playback_response: &Response) -> Option<Rect> {
        let hovered = ui.rect_contains_pointer(playback_response.rect);
        let currently_seeking = matches!(self.player_state.get(), PlayerState::Seeking(_));
        let is_stopped = matches!(self.player_state.get(), PlayerState::Stopped);
        let is_paused = matches!(self.player_state.get(), PlayerState::Paused);

        let seekbar_anim = ui
            .ctx()
            .animate_bool_with_time(
                playback_response.id.with("seekbar_anim_android"),
                hovered || currently_seeking || is_paused || is_stopped,
                0.2,
            )
            .min(1.0);

        if seekbar_anim <= 0.0 {
            return None;
        }

        let mut seekbar_rect = playback_response.rect.shrink2(vec2(14.0, 10.0));
        let mut fullseekbar_rect = seekbar_rect.clone();
        seekbar_rect.set_bottom(seekbar_rect.bottom());
        fullseekbar_rect.set_bottom(fullseekbar_rect.bottom());

        seekbar_rect.set_height(4.0);
        fullseekbar_rect.set_height(4.0);

        let seek_frac = match self.player_state.get_true() {
            PlayerState::Seeking(frac) => frac,
            _ => self.duration_frac(),
        };

        seekbar_rect.set_right(
            fullseekbar_rect.left()
                + (fullseekbar_rect.width().max(1.0) * seek_frac.min(1.0).max(0.0)),
        );

        let painter = ui.painter();
        painter.rect_filled(
            fullseekbar_rect,
            2.0,
            Color32::from_white_alpha((40.0 + seekbar_anim * 120.0) as u8),
        );
        painter.rect_filled(
            seekbar_rect,
            2.0,
            ui.visuals().selection.bg_fill.gamma_multiply(1.3),
        );

        Some(fullseekbar_rect)
    }

    fn process_errors(&mut self, ui: &mut Ui) {
        if let Ok(mut slot) = self.error_slot.lock() {
            if let Some(err) = slot.take() {
                ui.ctx().debug_painter().text(
                    ui.min_rect().left_bottom() + vec2(8.0, -8.0),
                    egui::Align2::LEFT_BOTTOM,
                    format!("Video error: {err}"),
                    FontId::proportional(12.0),
                    Color32::from_rgb(220, 80, 80),
                );
                self.player_state.set(PlayerState::Stopped);
            }
        }
    }

    /// Start playback from the beginning.
    pub fn start(&mut self) {
        let _ = self.command_tx.send(DecoderCommand::Stop);
        let _ = self.command_tx.send(DecoderCommand::Play);
        self.player_state.set(PlayerState::Playing);
        self.video_elapsed_ms.set(0);
    }

    /// Pause playback.
    pub fn pause(&mut self) {
        let _ = self.command_tx.send(DecoderCommand::Pause);
        self.player_state.set(PlayerState::Paused);
    }

    /// Resume playback.
    pub fn unpause(&mut self) {
        let _ = self.command_tx.send(DecoderCommand::Play);
        self.player_state.set(PlayerState::Playing);
    }

    /// Stop playback.
    pub fn stop(&mut self) {
        let _ = self.command_tx.send(DecoderCommand::Stop);
        self.player_state.set(PlayerState::Stopped);
    }

    /// Human readable duration.
    pub fn duration_text(&mut self) -> String {
        fn format_ms(ms: i64) -> String {
            let total_secs = (ms / 1000).max(0);
            let minutes = total_secs / 60;
            let seconds = total_secs % 60;
            format!("{minutes:02}:{seconds:02}")
        }

        let elapsed = self.video_elapsed_ms.get_true();
        format!("{} / {}", format_ms(elapsed), format_ms(self.duration_ms))
    }

    fn duration_frac(&mut self) -> f32 {
        if self.duration_ms <= 0 {
            return 0.0;
        }
        self.video_elapsed_ms.get_true() as f32 / self.duration_ms as f32
    }

    fn drain_new_frame(&mut self) {
        let frame_opt = self
            .pending_frames
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        if let Some(frame) = frame_opt {
            if frame.width == 0 || frame.height == 0 {
                return;
            }

            let image =
                ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.pixels);
            self.texture_handle.set(image, self.texture_options.clone());
            self.video_elapsed_ms.set(frame.timestamp_ms);
            self.width = frame.width as u32;
            self.height = frame.height as u32;
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        let _ = self.command_tx.send(DecoderCommand::Quit);
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

/// Decoder thread wrapper – owns the MediaCodec session.
struct DecoderThread {
    init: DecoderInit,
    session: DecoderSession,
    state: Cache<PlayerState>,
    elapsed: Cache<i64>,
    frames: Arc<Mutex<Option<FrameData>>>,
    command_rx: Receiver<DecoderCommand>,
    stop: Arc<AtomicBool>,
    playing: bool,
}

impl DecoderThread {
    fn spawn(
        path: String,
        state: Cache<PlayerState>,
        elapsed: Cache<i64>,
        frames: Arc<Mutex<Option<FrameData>>>,
        command_rx: Receiver<DecoderCommand>,
        stop: Arc<AtomicBool>,
    ) -> Result<Self> {
        let session = DecoderSession::new(&path)?;
        let init = session.init_meta();

        Ok(Self {
            init,
            session,
            state,
            elapsed,
            frames,
            command_rx,
            stop,
            playing: false,
        })
    }

    fn run(&mut self) -> Result<()> {
        loop {
            if self.stop.load(Ordering::SeqCst) {
                break;
            }

            while let Ok(cmd) = self.command_rx.try_recv() {
                match cmd {
                    DecoderCommand::Play => {
                        self.playing = true;
                        self.state.set(PlayerState::Playing);
                        self.session.ensure_running()?;
                    }
                    DecoderCommand::Pause => {
                        self.playing = false;
                        self.state.set(PlayerState::Paused);
                    }
                    DecoderCommand::Stop => {
                        self.playing = false;
                        self.session.reset()?;
                        self.elapsed.set(0);
                        self.state.set(PlayerState::Stopped);
                    }
                    DecoderCommand::SeekFraction(_f) => {
                        // Seeking not yet implemented on Android backend; ignore gracefully.
                    }
                    DecoderCommand::Quit => {
                        self.playing = false;
                        self.state.set(PlayerState::Stopped);
                        self.stop.store(true, Ordering::SeqCst);
                    }
                }
            }

            if !self.playing {
                thread::sleep(Duration::from_millis(16));
                continue;
            }

            let advanced = self.session.pump()?;
            if let Some(frame) = self.session.try_acquire_frame()? {
                if let Ok(mut slot) = self.frames.lock() {
                    *slot = Some(frame);
                }
            }

            if let Some(current_pts) = self.session.current_pts_ms {
                self.elapsed.set(current_pts);
            }

            if self.session.output_complete {
                self.state.set(PlayerState::EndOfFile);
                self.playing = false;
            }

            if !advanced {
                thread::sleep(Duration::from_millis(4));
            }
        }

        Ok(())
    }
}

struct DecoderSession {
    extractor: MediaExtractor,
    codec: MediaCodec,
    image_reader: ImageReader,
    video_track_index: usize,
    input_complete: bool,
    output_complete: bool,
    init_meta: DecoderInit,
    buffer_timeout: Duration,
    current_pts_ms: Option<i64>,
}

impl DecoderSession {
    fn new(path: &str) -> Result<Self> {
        let mut extractor = MediaExtractor::from_path(path)?;
        let (track_index, mut format) = extractor
            .video_track()
            .ok_or_else(|| anyhow!("no video track found"))?;

        let mime = format
            .str("mime")
            .ok_or_else(|| anyhow!("missing mime field"))?
            .to_owned();

        let width = format.i32("width").unwrap_or(0);
        let height = format.i32("height").unwrap_or(0);

        let duration_us = format.i64("durationUs").unwrap_or(0);
        let frame_rate = format.f32("frame-rate").unwrap_or(30.0);

        extractor.select_track(track_index)?;

        let usage = HardwareBufferUsage::CPU_READ_OFTEN | HardwareBufferUsage::GPU_SAMPLED_IMAGE;

        let reader = ImageReader::new_with_usage(
            width,
            height,
            ndk::media::image_reader::ImageFormat::RGBA_8888,
            usage,
            3,
        )
        .or_else(|_| {
            ImageReader::new(
                width,
                height,
                ndk::media::image_reader::ImageFormat::RGBA_8888,
                3,
            )
        })?;
        let window = reader.window()?;

        let codec = MediaCodec::from_decoder_type(&mime)
            .ok_or_else(|| anyhow!("failed to create decoder for mime {mime}"))?;
        codec.configure(&format, Some(&window), MediaCodecDirection::Decoder)?;
        codec.start()?;

        let init_meta = DecoderInit {
            width: width as u32,
            height: height as u32,
            duration_ms: duration_us / 1000,
            frame_rate,
        };

        Ok(Self {
            extractor,
            codec,
            image_reader: reader,
            video_track_index: track_index,
            input_complete: false,
            output_complete: false,
            init_meta,
            buffer_timeout: Duration::from_millis(10),
            current_pts_ms: None,
        })
    }

    fn init_meta(&self) -> DecoderInit {
        self.init_meta.clone()
    }

    fn ensure_running(&mut self) -> Result<()> {
        if self.output_complete {
            self.reset()?;
        }
        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        self.extractor.seek_to_start()?;
        self.codec.flush()?;
        self.input_complete = false;
        self.output_complete = false;
        self.current_pts_ms = None;
        Ok(())
    }

    fn pump(&mut self) -> Result<bool> {
        let mut progressed = false;

        if !self.input_complete {
            match self.codec.dequeue_input_buffer(self.buffer_timeout)? {
                DequeuedInputBufferResult::Buffer(mut buffer) => {
                    let slice_ptr = buffer.buffer_mut();
                    let len = slice_ptr.len();
                    let data = unsafe {
                        std::slice::from_raw_parts_mut(slice_ptr.as_mut_ptr() as *mut u8, len)
                    };

                    match self.extractor.read_sample_data(data)? {
                        Some(sample) => {
                            let time_us = sample.time_us as u64;
                            self.codec.queue_input_buffer(
                                buffer,
                                0,
                                sample.size,
                                time_us,
                                sample.flags,
                            )?;
                            self.extractor.advance();
                        }
                        None => {
                            self.codec.queue_input_buffer(
                                buffer,
                                0,
                                0,
                                0,
                                AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM as u32,
                            )?;
                            self.input_complete = true;
                        }
                    }
                    progressed = true;
                }
                DequeuedInputBufferResult::TryAgainLater => {}
            }
        }

        if !self.output_complete {
            match self.codec.dequeue_output_buffer(self.buffer_timeout)? {
                DequeuedOutputBufferInfoResult::Buffer(output) => {
                    let info = *output.info();
                    self.codec.release_output_buffer(output, true)?;
                    self.current_pts_ms = Some((info.presentation_time_us() / 1000).max(0));
                    if (info.flags() & (AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM as u32)) != 0 {
                        self.output_complete = true;
                    }
                    progressed = true;
                }
                DequeuedOutputBufferInfoResult::TryAgainLater => {}
                DequeuedOutputBufferInfoResult::OutputFormatChanged => {
                    progressed = true;
                }
                DequeuedOutputBufferInfoResult::OutputBuffersChanged => {
                    progressed = true;
                }
            }
        }

        Ok(progressed)
    }

    fn try_acquire_frame(&mut self) -> Result<Option<FrameData>> {
        match self.image_reader.acquire_latest_image() {
            Ok(AcquireResult::Image(image)) => {
                let width = image.width()? as usize;
                let height = image.height()? as usize;
                let plane = image.plane_data(0)?;
                let row_stride = image.plane_row_stride(0)? as usize;
                let pixel_stride = image.plane_pixel_stride(0)? as usize;

                if width == 0 || height == 0 {
                    return Ok(None);
                }

                let mut pixels = vec![0u8; width * height * 4];

                if pixel_stride == 4 {
                    for y in 0..height {
                        let src_start = y * row_stride;
                        let dst_start = y * width * 4;
                        let copy_len = width * 4;
                        pixels[dst_start..dst_start + copy_len]
                            .copy_from_slice(&plane[src_start..src_start + copy_len]);
                    }
                } else {
                    // Fallback per-pixel copy.
                    for y in 0..height {
                        for x in 0..width {
                            let src_index = y * row_stride + x * pixel_stride;
                            let dst_index = (y * width + x) * 4;
                            pixels[dst_index..dst_index + 4]
                                .copy_from_slice(&plane[src_index..src_index + 4]);
                        }
                    }
                }

                let timestamp_ms = self.current_pts_ms.unwrap_or(0);

                Ok(Some(FrameData {
                    pixels,
                    width,
                    height,
                    timestamp_ms,
                }))
            }
            Ok(AcquireResult::NoBufferAvailable) => Ok(None),
            Ok(AcquireResult::MaxImagesAcquired) => Ok(None),
            Err(err) => Err(anyhow!("failed to acquire image: {err:?}")),
        }
    }
}

struct Sample {
    size: usize,
    time_us: i64,
    flags: u32,
}

/// Minimal AMediaExtractor wrapper (the NDK crate does not expose it yet).
struct MediaExtractor {
    inner: NonNull<ffi::AMediaExtractor>,
}

impl MediaExtractor {
    fn from_path(path: &str) -> Result<Self> {
        unsafe {
            let ptr = ffi::AMediaExtractor_new();
            let inner = NonNull::new(ptr).ok_or_else(|| anyhow!("AMediaExtractor_new failed"))?;
            let mut extractor = Self { inner };
            extractor.set_data_source(path)?;
            Ok(extractor)
        }
    }

    fn set_data_source(&mut self, path: &str) -> Result<()> {
        let c_path = CString::new(path).map_err(|_| anyhow!("invalid path"))?;
        let status =
            unsafe { ffi::AMediaExtractor_setDataSource(self.inner.as_ptr(), c_path.as_ptr()) };
        media_status_to_result(status)?;
        Ok(())
    }

    fn track_count(&self) -> usize {
        unsafe { ffi::AMediaExtractor_getTrackCount(self.inner.as_ptr()) }
    }

    fn video_track(&mut self) -> Option<(usize, MediaFormat)> {
        for index in 0..self.track_count() {
            if let Some(mut format) = self.track_format(index) {
                if let Some(mime) = format.str("mime") {
                    if mime.starts_with("video/") {
                        return Some((index, format));
                    }
                }
            }
        }
        None
    }

    fn track_format(&self, index: usize) -> Option<MediaFormat> {
        let ptr = unsafe { ffi::AMediaExtractor_getTrackFormat(self.inner.as_ptr(), index) };
        NonNull::new(ptr).map(|inner| unsafe { MediaFormat::from_ptr(inner) })
    }

    fn select_track(&mut self, index: usize) -> Result<()> {
        let status = unsafe { ffi::AMediaExtractor_selectTrack(self.inner.as_ptr(), index) };
        media_status_to_result(status)?;
        Ok(())
    }

    fn read_sample_data(&mut self, buffer: &mut [u8]) -> Result<Option<Sample>> {
        let read = unsafe {
            ffi::AMediaExtractor_readSampleData(
                self.inner.as_ptr(),
                buffer.as_mut_ptr(),
                buffer.len(),
            )
        };

        if read < 0 {
            return Ok(None);
        }

        let flags = unsafe { ffi::AMediaExtractor_getSampleFlags(self.inner.as_ptr()) };
        let time = unsafe { ffi::AMediaExtractor_getSampleTime(self.inner.as_ptr()) };

        Ok(Some(Sample {
            size: read as usize,
            time_us: time,
            flags,
        }))
    }

    fn advance(&mut self) {
        unsafe {
            ffi::AMediaExtractor_advance(self.inner.as_ptr());
        }
    }

    fn seek_to_start(&mut self) -> Result<()> {
        // SEEK_CLOSEST_SYNC = 2
        let status = unsafe {
            ffi::AMediaExtractor_seekTo(
                self.inner.as_ptr(),
                0,
                ffi::SeekMode::AMEDIAEXTRACTOR_SEEK_CLOSEST_SYNC,
            )
        };
        media_status_to_result(status)?;
        Ok(())
    }
}

impl Drop for MediaExtractor {
    fn drop(&mut self) {
        unsafe { ffi::AMediaExtractor_delete(self.inner.as_ptr()) };
    }
}

fn media_status_to_result(status: ffi::media_status_t) -> Result<()> {
    if status == ffi::media_status_t::AMEDIA_OK {
        Ok(())
    } else {
        Err(anyhow!("{:?}", MediaError::from(status.0)))
    }
}
