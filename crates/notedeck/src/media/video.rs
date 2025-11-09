#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
mod imp {
    use crate::imgcache::MediaCache;
    use crate::media::load_texture_checked;
    use crate::{Error, Result};
    use egui::{ColorImage, Context, TextureHandle};
    use poll_promise::Promise;
    use rodio::{OutputStream, OutputStreamHandle, Sink, Source};
    use rsmpeg::avcodec::{AVCodec, AVCodecContext};
    use rsmpeg::avformat::AVFormatContextInput;
    use rsmpeg::avutil::{
        hwdevice_find_type_by_name, hwdevice_get_type_name, AVFrame, AVHWDeviceContext,
        AVHWDeviceType, AVPixelFormat,
    };
    use rsmpeg::ffi;
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::ffi::CString;
    use std::fs;
    use std::io::{Read, Write};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, LazyLock};
    use std::time::Duration;
    use tracing::{debug, info, warn};
    use ureq;

    const MAX_VIDEO_BYTES: usize = 150 * 1024 * 1024;
    const MAX_VIDEO_FRAMES: usize = 600;
    const MAX_VIDEO_WIDTH: u32 = 480;
    const MAX_TARGET_FPS: f64 = 18.0;
    const MAX_CONCURRENT_LOADS: usize = 3;

    // Cache the preferred hardware device type (determined once at startup)
    static PREFERRED_HW_DEVICE: LazyLock<Option<AVHWDeviceType>> = LazyLock::new(|| {
        #[cfg(target_os = "macos")]
        {
            let device_type = hwdevice_find_type_by_name(c"videotoolbox");
            if device_type != ffi::AV_HWDEVICE_TYPE_NONE {
                info!("Hardware acceleration available: VideoToolbox");
                return Some(device_type);
            }
        }

        #[cfg(target_os = "windows")]
        {
            // Try D3D11VA first (modern Windows)
            let device_type = hwdevice_find_type_by_name(c"d3d11va");
            if device_type != ffi::AV_HWDEVICE_TYPE_NONE {
                info!("Hardware acceleration available: D3D11VA");
                return Some(device_type);
            }
            // Fall back to DXVA2 (legacy)
            let device_type = hwdevice_find_type_by_name(c"dxva2");
            if device_type != ffi::AV_HWDEVICE_TYPE_NONE {
                info!("Hardware acceleration available: DXVA2");
                return Some(device_type);
            }
        }

        #[cfg(target_os = "linux")]
        {
            // Try VAAPI for Intel/AMD
            let device_type = hwdevice_find_type_by_name(c"vaapi");
            if device_type != ffi::AV_HWDEVICE_TYPE_NONE {
                info!("Hardware acceleration available: VAAPI");
                return Some(device_type);
            }
            // Try CUDA for NVIDIA
            let device_type = hwdevice_find_type_by_name(c"cuda");
            if device_type != ffi::AV_HWDEVICE_TYPE_NONE {
                info!("Hardware acceleration available: CUDA");
                return Some(device_type);
            }
        }

        info!("Hardware acceleration not available, using software decoding");
        None
    });

    // Platform-specific hardware acceleration selection (cached)
    fn get_preferred_hw_device_type() -> Option<AVHWDeviceType> {
        *PREFERRED_HW_DEVICE
    }

    #[derive(Debug, Clone, Copy)]
    pub struct VideoClipMeta {
        pub width: u32,
        pub height: u32,
        pub duration: Duration,
        pub frame_interval: Duration,
        pub frame_count: usize,
    }

    impl VideoClipMeta {
        pub fn aspect_ratio(&self) -> f32 {
            if self.height == 0 {
                1.0
            } else {
                self.width as f32 / self.height as f32
            }
        }

        pub fn duration_secs(&self) -> f32 {
            self.duration.as_secs_f32()
        }

        pub fn frame_interval_secs(&self) -> f32 {
            self.frame_interval.as_secs_f32().max(f32::EPSILON)
        }
    }

    #[derive(Debug, Clone)]
    pub enum VideoClipState {
        Unsupported,
        NotLoaded,
        Queued,
        Loading,
        Ready(VideoClipMeta),
        Error(String),
    }

    #[derive(Debug, Clone, Default)]
    pub struct VideoPlaybackState {
        playing: bool,
        looping: bool,
        current_frame: usize,
        last_time: Option<f64>,
    }

    impl VideoPlaybackState {
        pub fn update(&mut self, now: f64, meta: &VideoClipMeta) {
            if !self.playing || meta.frame_count == 0 {
                self.last_time = Some(now);
                return;
            }

            let last = self.last_time.unwrap_or(now);
            let delta = now - last;
            if delta <= 0.0 {
                return;
            }

            let interval = meta.frame_interval_secs() as f64;
            if interval <= 0.0 {
                return;
            }

            let frames_to_advance = (delta / interval).floor() as usize;
            if frames_to_advance == 0 {
                return;
            }

            self.current_frame += frames_to_advance;
            if self.current_frame >= meta.frame_count {
                if self.looping {
                    self.current_frame %= meta.frame_count;
                } else {
                    self.current_frame = 0;
                    self.playing = false;
                }
            }

            self.last_time = Some(last + (frames_to_advance as f64 * interval));
        }

        pub fn toggle(&mut self, now: f64) {
            self.playing = !self.playing;
            self.last_time = Some(now);
        }

        pub fn set_playing(&mut self, playing: bool, now: f64) {
            self.playing = playing;
            self.last_time = Some(now);
        }

        pub fn seek_seconds(&mut self, seconds: f32, meta: &VideoClipMeta) {
            if meta.frame_count == 0 {
                return;
            }

            let clamped = seconds.clamp(0.0, meta.duration_secs()).max(0.0);
            let target_frame = (clamped / meta.frame_interval_secs()).round() as usize;
            self.current_frame = target_frame.min(meta.frame_count.saturating_sub(1));
            self.last_time = None;
        }

        pub fn current_frame(&self, total: usize) -> usize {
            self.current_frame.min(total.saturating_sub(1))
        }

        pub fn is_playing(&self) -> bool {
            self.playing
        }

        pub fn current_time(&self, meta: &VideoClipMeta) -> f32 {
            (self.current_frame.min(meta.frame_count)) as f32 * meta.frame_interval_secs()
        }
    }

    struct VideoFrame {
        image: ColorImage,
        texture: Option<TextureHandle>,
    }

    #[derive(Clone)]
    struct AudioClip {
        samples: Arc<Vec<f32>>,
        sample_rate: u32,
        channels: u16,
    }

    impl AudioClip {
        fn new(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Self {
            Self {
                samples: Arc::new(samples),
                sample_rate,
                channels,
            }
        }
    }

    struct VideoClip {
        key: String,
        frames: Vec<VideoFrame>,
        meta: VideoClipMeta,
        audio: Option<AudioClip>,
    }

    impl VideoClip {
        fn frame_texture(&mut self, ctx: &Context, index: usize) -> Option<&TextureHandle> {
            let frame = self.frames.get_mut(index)?;
            Self::ensure_frame_texture(ctx, &self.key, frame, index);
            frame.texture.as_ref()
        }

        fn preload_textures(&mut self, ctx: &Context, start_index: usize, count: usize) {
            let end_index = (start_index + count).min(self.frames.len());
            for index in start_index..end_index {
                if let Some(frame) = self.frames.get_mut(index) {
                    Self::ensure_frame_texture(ctx, &self.key, frame, index);
                }
            }
        }

        fn ensure_frame_texture(ctx: &Context, key: &str, frame: &mut VideoFrame, index: usize) {
            if frame.texture.is_none() {
                frame.texture = Some(load_texture_checked(
                    ctx,
                    format!("video:{key}:{index}"),
                    frame.image.clone(),
                    Default::default(),
                ));
            }
        }
    }

    enum VideoEntry {
        PendingFull(Promise<Option<Result<VideoClip>>>),
        Ready(VideoClip),
        Error(String),
    }

    struct AudioEngine {
        _stream: OutputStream,
        handle: OutputStreamHandle,
        active: HashMap<String, Sink>,
    }

    impl AudioEngine {
        fn new() -> Option<Self> {
            let (stream, handle) = OutputStream::try_default().ok()?;
            Some(Self {
                _stream: stream,
                handle,
                active: HashMap::new(),
            })
        }

        fn play(&mut self, url: &str, clip: &AudioClip, start_secs: f32) {
            self.stop(url);

            if clip.samples.is_empty() {
                return;
            }

            let start_sample = ((start_secs.max(0.0) * clip.sample_rate as f32) as usize)
                .saturating_mul(clip.channels as usize)
                .min(clip.samples.len());

            if let Ok(sink) = Sink::try_new(&self.handle) {
                let source = VideoAudioSource::new(
                    clip.samples.clone(),
                    clip.channels,
                    clip.sample_rate,
                    start_sample,
                );
                sink.append(source);
                self.active.insert(url.to_owned(), sink);
            }
        }

        fn stop(&mut self, url: &str) {
            if let Some(sink) = self.active.remove(url) {
                sink.stop();
            }
        }
    }

    struct VideoAudioSource {
        samples: Arc<Vec<f32>>,
        channels: u16,
        sample_rate: u32,
        position: usize,
    }

    impl VideoAudioSource {
        fn new(samples: Arc<Vec<f32>>, channels: u16, sample_rate: u32, start: usize) -> Self {
            let len = samples.len();
            Self {
                samples,
                channels,
                sample_rate,
                position: start.min(len),
            }
        }
    }

    impl Iterator for VideoAudioSource {
        type Item = f32;

        fn next(&mut self) -> Option<Self::Item> {
            if self.position >= self.samples.len() {
                return None;
            }

            let sample = self.samples[self.position];
            self.position += 1;
            Some(sample)
        }
    }

    impl Source for VideoAudioSource {
        fn current_frame_len(&self) -> Option<usize> {
            None
        }

        fn channels(&self) -> u16 {
            self.channels
        }

        fn sample_rate(&self) -> u32 {
            self.sample_rate.max(1)
        }

        fn total_duration(&self) -> Option<Duration> {
            if self.channels == 0 || self.sample_rate == 0 {
                return None;
            }
            let remaining = self.samples.len().saturating_sub(self.position);
            let frames = remaining as f32 / self.channels as f32;
            Some(Duration::from_secs_f32(frames / self.sample_rate as f32))
        }
    }

    pub struct VideoStore {
        cache_dir: PathBuf,
        entries: HashMap<String, VideoEntry>,
        playback: HashMap<String, VideoPlaybackState>,
        audio_engine: Option<AudioEngine>,
        auto_play_pending: HashSet<String>,
        pending_queue: VecDeque<String>,
        muted: HashSet<String>,
    }

    impl VideoStore {
        pub fn new(cache_dir: PathBuf) -> Self {
            let _ = fs::create_dir_all(&cache_dir);
            let audio_engine = AudioEngine::new();
            if audio_engine.is_none() {
                warn!("Failed to initialize audio output. Inline video will be muted.");
            }
            Self {
                cache_dir,
                entries: HashMap::new(),
                playback: HashMap::new(),
                audio_engine,
                auto_play_pending: HashSet::new(),
                pending_queue: VecDeque::new(),
                muted: HashSet::new(),
            }
        }

        pub fn clip_state(&mut self, url: &str) -> VideoClipState {
            if let Some(entry) = self.entries.get_mut(url) {
                Self::drive_entry(entry);
                let state = match entry {
                    VideoEntry::PendingFull(_) => VideoClipState::Loading,
                    VideoEntry::Ready(clip) => VideoClipState::Ready(clip.meta),
                    VideoEntry::Error(err) => VideoClipState::Error(err.clone()),
                };

                // Process queue after driving entries (a slot may have freed up)
                self.process_queue();

                return state;
            }

            // Check if URL is in the pending queue
            if self.pending_queue.contains(&url.to_owned()) {
                return VideoClipState::Queued;
            }

            VideoClipState::NotLoaded
        }

        pub fn request_video_load(&mut self, url: &str) {
            if self.entries.contains_key(url) {
                return;
            }

            // Don't queue duplicates
            if self.pending_queue.contains(&url.to_owned()) {
                return;
            }

            let loading_count = self
                .entries
                .values()
                .filter(|e| matches!(e, VideoEntry::PendingFull(_)))
                .count();

            if loading_count >= MAX_CONCURRENT_LOADS {
                // Queue the request instead of silently dropping it
                self.pending_queue.push_back(url.to_owned());
                return;
            }

            self.entries.insert(
                url.to_owned(),
                Self::spawn_full_loader(url.to_owned(), self.cache_dir.clone()),
            );
        }

        pub fn thumbnail_texture(&mut self, ctx: &Context, url: &str) -> Option<TextureHandle> {
            let entry = self.entries.get_mut(url)?;
            match entry {
                VideoEntry::Ready(clip) => clip.frame_texture(ctx, 0).cloned(),
                _ => None,
            }
        }

        pub fn request_full_video(&mut self, url: &str) {
            self.request_video_load(url);
            self.auto_play_pending.insert(url.to_owned());
        }

        pub fn should_auto_play(&mut self, url: &str) -> bool {
            self.auto_play_pending.remove(url)
        }

        pub fn frame_texture(
            &mut self,
            ctx: &Context,
            url: &str,
            index: usize,
        ) -> Option<TextureHandle> {
            let entry = self.entries.get_mut(url)?;
            match entry {
                VideoEntry::Ready(clip) => clip.frame_texture(ctx, index).cloned(),
                _ => None,
            }
        }

        pub fn preload_upcoming_textures(
            &mut self,
            ctx: &Context,
            url: &str,
            current_frame: usize,
        ) {
            const PRELOAD_FRAME_COUNT: usize = 5;

            if let Some(VideoEntry::Ready(clip)) = self.entries.get_mut(url) {
                clip.preload_textures(ctx, current_frame, PRELOAD_FRAME_COUNT);
            }
        }

        pub fn meta(&self, url: &str) -> Option<VideoClipMeta> {
            match self.entries.get(url) {
                Some(VideoEntry::Ready(clip)) => Some(clip.meta),
                _ => None,
            }
        }

        pub fn playback_mut(&mut self, url: &str) -> &mut VideoPlaybackState {
            self.playback.entry(url.to_owned()).or_default()
        }

        pub fn is_audio_active(&self, url: &str) -> bool {
            self.audio_engine
                .as_ref()
                .is_some_and(|engine| engine.active.contains_key(url))
        }

        pub fn play_audio_from(&mut self, url: &str, start_secs: f32) {
            let Some(engine) = self.audio_engine.as_mut() else {
                return;
            };
            let Some(VideoEntry::Ready(clip)) = self.entries.get(url) else {
                return;
            };
            let Some(audio) = clip.audio.as_ref() else {
                return;
            };
            engine.play(url, audio, start_secs);
        }

        pub fn stop_audio(&mut self, url: &str) {
            if let Some(engine) = self.audio_engine.as_mut() {
                engine.stop(url);
            }
        }

        pub fn is_muted(&self, url: &str) -> bool {
            self.muted.contains(url)
        }

        pub fn set_muted(&mut self, url: &str, muted: bool) {
            if muted {
                self.muted.insert(url.to_owned());
                self.stop_audio(url);
            } else {
                self.muted.remove(url);
            }
        }

        pub fn clear_cache(&mut self) -> std::io::Result<()> {
            self.entries.clear();
            self.playback.clear();
            self.auto_play_pending.clear();
            self.pending_queue.clear();
            self.muted.clear();
            if let Some(engine) = self.audio_engine.as_mut() {
                for (_, sink) in engine.active.drain() {
                    sink.stop();
                }
            }
            if self.cache_dir.exists() {
                fs::remove_dir_all(&self.cache_dir)?;
            }
            fs::create_dir_all(&self.cache_dir)?;
            Ok(())
        }

        fn process_queue(&mut self) {
            // Start loading queued videos if slots are available
            loop {
                let loading_count = self
                    .entries
                    .values()
                    .filter(|e| matches!(e, VideoEntry::PendingFull(_)))
                    .count();

                if loading_count >= MAX_CONCURRENT_LOADS {
                    break;
                }

                let Some(url) = self.pending_queue.pop_front() else {
                    break;
                };

                // Double-check it's not already loaded (edge case)
                if !self.entries.contains_key(&url) {
                    self.entries.insert(
                        url.clone(),
                        Self::spawn_full_loader(url, self.cache_dir.clone()),
                    );
                }
            }
        }

        fn spawn_full_loader(url: String, cache_dir: PathBuf) -> VideoEntry {
            let promise = Promise::spawn_thread("video-full-loader", move || {
                Some(load_or_decode_video(&url, &cache_dir))
            });
            VideoEntry::PendingFull(promise)
        }

        fn drive_entry(entry: &mut VideoEntry) {
            if let VideoEntry::PendingFull(promise) = entry {
                let Some(res) = promise.ready_mut() else {
                    return;
                };

                let Some(outcome) = res.take() else {
                    *entry = VideoEntry::Error("Video promise already taken".into());
                    return;
                };

                match outcome {
                    Ok(clip) => *entry = VideoEntry::Ready(clip),
                    Err(err) => *entry = VideoEntry::Error(err.to_string()),
                }
            }
        }
    }

    fn load_or_decode_video(url: &str, cache_dir: &Path) -> Result<VideoClip> {
        let key = MediaCache::key(url);
        let path = cache_dir.join(&key);

        if !path.exists() {
            download_video(url, &path)?;
        }

        let result = decode_video(&path, &key);

        if let Err(e) = &result {
            tracing::error!("Video decode failed for {}: {}", url, e);
        }

        result
    }

    fn download_video(url: &str, path: &Path) -> Result<()> {
        let response = ureq::get(url)
            .set("User-Agent", "NotedeckVideo/1.0")
            .call()
            .map_err(|err| Error::Generic(format!("Video download failed: {err}")))?;

        if !(200..400).contains(&response.status()) {
            return Err(Error::Generic(format!(
                "Video request returned {}",
                response.status()
            )));
        }

        if let Some(content_type) = response.header("content-type") {
            if !content_type.starts_with("video")
                && !content_type.starts_with("application/octet-stream")
            {
                return Err(Error::Generic(format!(
                    "Unsupported video mime type {content_type}"
                )));
            }
        }

        let mut reader = response.into_reader();
        let tmp_path = path.with_extension("tmp");

        if let Some(parent) = tmp_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut file = fs::File::create(&tmp_path)?;
        let mut downloaded: usize = 0;
        let mut buf = [0u8; 8192];

        loop {
            let read = reader.read(&mut buf)?;
            if read == 0 {
                break;
            }
            downloaded += read;
            if downloaded > MAX_VIDEO_BYTES {
                let msg = format!(
                    "Video too large for inline playback (exceeds {} MB limit)",
                    MAX_VIDEO_BYTES / (1024 * 1024)
                );
                tracing::warn!("{} - URL: {}", msg, url);
                return Err(Error::Generic(msg));
            }
            file.write_all(&buf[..read])?;
        }

        fs::rename(tmp_path, path)?;
        Ok(())
    }

    // Hardware pixel format storage for callback (thread-local for safety)
    // Each decoder thread gets its own storage to avoid interference
    thread_local! {
        static HW_PIX_FMT: RefCell<Option<AVPixelFormat>> = const { RefCell::new(None) };
    }

    unsafe extern "C" fn get_hw_format(
        _ctx: *mut ffi::AVCodecContext,
        pix_fmts: *const ffi::AVPixelFormat,
    ) -> ffi::AVPixelFormat {
        let mut current = pix_fmts;
        let hw_pix_fmt = HW_PIX_FMT.with(|f| *f.borrow());

        unsafe {
            while *current != ffi::AV_PIX_FMT_NONE {
                if let Some(target) = hw_pix_fmt {
                    if *current == target {
                        return target;
                    }
                }
                current = current.add(1);
            }
        }

        ffi::AV_PIX_FMT_NONE
    }

    // Helper function to format device type name
    fn device_type_name(device_type: AVHWDeviceType) -> &'static str {
        hwdevice_get_type_name(device_type)
            .and_then(|c| c.to_str().ok())
            .unwrap_or("unknown")
    }

    fn decode_video(path: &Path, key: &str) -> Result<VideoClip> {
        let path_str = path
            .to_str()
            .ok_or_else(|| Error::Generic("Invalid path encoding".into()))?;
        let path_cstr =
            CString::new(path_str).map_err(|e| Error::Generic(format!("Invalid path: {}", e)))?;

        // Open input file
        let mut input_ctx = AVFormatContextInput::open(&path_cstr)
            .map_err(|e| Error::Generic(format!("Failed to open video file: {}", e)))?;

        // Find video stream
        let (video_stream_idx, decoder) = input_ctx
            .find_best_stream(ffi::AVMEDIA_TYPE_VIDEO)
            .map_err(|e| Error::Generic(format!("Failed to find video stream: {}", e)))?
            .ok_or_else(|| Error::Generic("No video stream found".into()))?;

        let video_stream = &input_ctx.streams()[video_stream_idx];

        // Create codec context
        let mut dec_ctx = AVCodecContext::new(&decoder);
        dec_ctx
            .apply_codecpar(&video_stream.codecpar())
            .map_err(|e| Error::Generic(format!("Failed to apply codec parameters: {}", e)))?;

        // Try hardware acceleration
        let hw_device_ctx = try_init_hw_decoder(&mut dec_ctx, &decoder);

        // Open decoder
        dec_ctx
            .open(None)
            .map_err(|e| Error::Generic(format!("Failed to open decoder: {}", e)))?;

        let src_width = dec_ctx.width as u32;
        let src_height = dec_ctx.height as u32;

        if src_width == 0 || src_height == 0 {
            return Err(Error::Generic(
                "Video has invalid dimensions (width or height is zero)".into(),
            ));
        }

        let (dst_width, dst_height) = scaled_dimensions(src_width, src_height);

        // Calculate frame rate and stride
        let src_fps = fps_from_stream(video_stream).max(1.0);
        let stride = if src_fps > MAX_TARGET_FPS {
            (src_fps / MAX_TARGET_FPS).round().max(1.0) as usize
        } else {
            1
        };
        let effective_fps = (src_fps / stride as f64).max(1.0);
        let frame_interval = Duration::from_secs_f64(1.0 / effective_fps);

        // Find audio stream and set up audio decoder
        let mut audio_decoder: Option<(usize, AVCodecContext)> = None;
        if let Some((audio_idx, audio_codec)) = input_ctx
            .find_best_stream(ffi::AVMEDIA_TYPE_AUDIO)
            .ok()
            .flatten()
        {
            let mut audio_ctx = AVCodecContext::new(&audio_codec);
            if let Err(e) = audio_ctx.apply_codecpar(&input_ctx.streams()[audio_idx].codecpar()) {
                warn!("Failed to apply audio codec parameters: {}", e);
            } else if let Err(e) = audio_ctx.open(None) {
                warn!("Failed to open audio decoder: {}", e);
            } else {
                audio_decoder = Some((audio_idx, audio_ctx));
            }
        }

        let mut frames = Vec::new();
        let mut audio_samples: Vec<f32> = Vec::new();
        let mut frame_index = 0usize;
        let mut video_done = false;

        let audio_sample_rate;
        let audio_channels;

        if let Some((_, ref ctx)) = audio_decoder {
            audio_sample_rate = ctx.sample_rate as u32;
            audio_channels = ctx.ch_layout.nb_channels as u16;
        } else {
            audio_sample_rate = 0;
            audio_channels = 0;
        }

        // Process packets
        while let Some(packet) = input_ctx
            .read_packet()
            .map_err(|e| Error::Generic(format!("Failed to read packet: {}", e)))?
        {
            let stream_idx = packet.stream_index as usize;

            if stream_idx == video_stream_idx && !video_done {
                if let Err(e) = dec_ctx.send_packet(Some(&packet)) {
                    warn!("Failed to send video packet to decoder: {}", e);
                    continue;
                }

                loop {
                    let frame = match dec_ctx.receive_frame() {
                        Ok(f) => f,
                        Err(rsmpeg::error::RsmpegError::DecoderDrainError)
                        | Err(rsmpeg::error::RsmpegError::DecoderFlushedError) => break,
                        Err(e) => {
                            warn!("Failed to receive video frame: {}", e);
                            break;
                        }
                    };

                    if frame_index % stride == 0 {
                        if let Some(image) = process_video_frame(
                            &frame,
                            hw_device_ctx.as_ref(),
                            dst_width,
                            dst_height,
                        ) {
                            frames.push(image);
                            if frames.len() >= MAX_VIDEO_FRAMES {
                                video_done = true;
                                break;
                            }
                        }
                    }
                    frame_index += 1;
                }
            } else if let Some((audio_idx, ref mut audio_ctx)) = audio_decoder {
                if stream_idx == audio_idx {
                    if let Err(e) = audio_ctx.send_packet(Some(&packet)) {
                        warn!("Failed to send audio packet: {}", e);
                        continue;
                    }

                    loop {
                        let frame = match audio_ctx.receive_frame() {
                            Ok(f) => f,
                            Err(rsmpeg::error::RsmpegError::DecoderDrainError)
                            | Err(rsmpeg::error::RsmpegError::DecoderFlushedError) => break,
                            Err(e) => {
                                warn!("Failed to receive audio frame: {}", e);
                                break;
                            }
                        };

                        extract_audio_samples(&frame, &mut audio_samples);
                    }
                }
            }
        }

        // Flush video decoder
        let _ = dec_ctx.send_packet(None);
        loop {
            let frame = match dec_ctx.receive_frame() {
                Ok(f) => f,
                Err(_) => break,
            };

            if frame_index % stride == 0 {
                if let Some(image) =
                    process_video_frame(&frame, hw_device_ctx.as_ref(), dst_width, dst_height)
                {
                    frames.push(image);
                    if frames.len() >= MAX_VIDEO_FRAMES {
                        break;
                    }
                }
            }
            frame_index += 1;
        }

        // Flush audio decoder
        if let Some((_, ref mut audio_ctx)) = audio_decoder {
            let _ = audio_ctx.send_packet(None);
            loop {
                let frame = match audio_ctx.receive_frame() {
                    Ok(f) => f,
                    Err(_) => break,
                };

                extract_audio_samples(&frame, &mut audio_samples);
            }
        }

        if frames.is_empty() {
            return Err(Error::Generic(
                "Unable to decode any frames from video".into(),
            ));
        }

        let duration = frame_interval.mul_f32(frames.len() as f32);
        let meta = VideoClipMeta {
            width: dst_width,
            height: dst_height,
            duration,
            frame_interval,
            frame_count: frames.len(),
        };

        let audio = if audio_samples.is_empty() || audio_channels == 0 {
            None
        } else {
            Some(AudioClip::new(
                audio_samples,
                audio_sample_rate,
                audio_channels,
            ))
        };

        let clip = VideoClip {
            key: key.to_owned(),
            frames: frames
                .into_iter()
                .map(|image| VideoFrame {
                    image,
                    texture: None,
                })
                .collect(),
            meta,
            audio,
        };

        Ok(clip)
    }

    fn try_init_hw_decoder(
        dec_ctx: &mut AVCodecContext,
        decoder: &AVCodec,
    ) -> Option<AVHWDeviceContext> {
        let device_type = get_preferred_hw_device_type()?;
        let type_name = device_type_name(device_type);

        // Create hardware device context
        let hw_device_ctx = match AVHWDeviceContext::create(device_type, None, None, 0) {
            Ok(ctx) => ctx,
            Err(e) => {
                warn!(
                    "Failed to create hardware device context for {}: {}",
                    type_name, e
                );
                return None;
            }
        };

        // Find compatible hardware pixel format
        let mut hw_pix_fmt = None;
        for i in 0.. {
            let Some(config) = decoder.hw_config(i) else {
                break;
            };
            if config.methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 != 0
                && config.device_type == device_type
            {
                hw_pix_fmt = Some(config.pix_fmt);
                break;
            }
        }

        let Some(pix_fmt) = hw_pix_fmt else {
            warn!(
                "Decoder {} does not support hardware device type {}",
                decoder.name().to_string_lossy(),
                type_name
            );
            return None;
        };

        // Store pixel format for callback in thread-local storage
        HW_PIX_FMT.with(|f| *f.borrow_mut() = Some(pix_fmt));

        // Set hardware device context
        dec_ctx.set_hw_device_ctx(hw_device_ctx.clone());
        dec_ctx.set_get_format(Some(get_hw_format));

        debug!("Hardware decoder initialized: {}", type_name);

        Some(hw_device_ctx)
    }

    fn process_video_frame(
        frame: &AVFrame,
        hw_device_ctx: Option<&AVHWDeviceContext>,
        dst_width: u32,
        dst_height: u32,
    ) -> Option<ColorImage> {
        // Transfer from hardware to software if needed
        let sw_frame = if hw_device_ctx.is_some() && is_hw_frame(frame) {
            let mut sw_frame = AVFrame::new();
            if let Err(e) = sw_frame.hwframe_transfer_data(frame) {
                warn!("Failed to transfer frame from hardware: {}", e);
                return None;
            }
            sw_frame
        } else {
            frame.clone()
        };

        // Convert to RGBA
        let rgba_frame = match convert_to_rgba(&sw_frame, dst_width as i32, dst_height as i32) {
            Ok(f) => f,
            Err(e) => {
                warn!("Failed to convert frame to RGBA: {}", e);
                return None;
            }
        };

        Some(frame_to_color_image(&rgba_frame))
    }

    fn is_hw_frame(frame: &AVFrame) -> bool {
        let format = frame.format;
        format == ffi::AV_PIX_FMT_VIDEOTOOLBOX
            || format == ffi::AV_PIX_FMT_VAAPI
            || format == ffi::AV_PIX_FMT_CUDA
            || format == ffi::AV_PIX_FMT_D3D11
            || format == ffi::AV_PIX_FMT_DXVA2_VLD
            || format == ffi::AV_PIX_FMT_QSV
    }

    fn convert_to_rgba(frame: &AVFrame, dst_width: i32, dst_height: i32) -> Result<AVFrame> {
        use rsmpeg::swscale::SwsContext;

        let mut sws_ctx = SwsContext::get_context(
            frame.width,
            frame.height,
            frame.format,
            dst_width,
            dst_height,
            ffi::AV_PIX_FMT_RGBA,
            ffi::SWS_BILINEAR,
            None, // src_filter
            None, // dst_filter
            None, // param
        )
        .ok_or_else(|| Error::Generic("Failed to create scaler".into()))?;

        let mut rgba_frame = AVFrame::new();
        rgba_frame.set_format(ffi::AV_PIX_FMT_RGBA);
        rgba_frame.set_width(dst_width);
        rgba_frame.set_height(dst_height);
        rgba_frame
            .alloc_buffer()
            .map_err(|e| Error::Generic(format!("Failed to allocate frame buffer: {}", e)))?;

        sws_ctx
            .scale_frame(frame, 0, frame.height, &mut rgba_frame)
            .map_err(|e| Error::Generic(format!("Failed to scale frame: {}", e)))?;

        Ok(rgba_frame)
    }

    fn frame_to_color_image(frame: &AVFrame) -> ColorImage {
        let width = frame.width as usize;
        let height = frame.height as usize;

        let data = frame.data[0];
        let linesize = frame.linesize[0] as usize;

        let mut pixels = vec![0u8; width * height * 4];

        for y in 0..height {
            let src_offset = y * linesize;
            let dst_offset = y * width * 4;
            let row_len = width * 4;

            unsafe {
                std::ptr::copy_nonoverlapping(
                    data.add(src_offset),
                    pixels.as_mut_ptr().add(dst_offset),
                    row_len,
                );
            }
        }

        ColorImage::from_rgba_unmultiplied([width, height], &pixels)
    }

    fn extract_audio_samples(frame: &AVFrame, samples: &mut Vec<f32>) {
        // Convert audio to f32 planar format
        use rsmpeg::swresample::SwrContext;

        let nb_samples = frame.nb_samples;
        if nb_samples == 0 {
            return;
        }

        let in_sample_rate = frame.sample_rate;
        let out_sample_rate = in_sample_rate; // Keep original sample rate
        let in_ch_layout = &frame.ch_layout;
        let out_ch_layout = in_ch_layout; // Keep original channel layout

        let swr_ctx = match SwrContext::new(
            out_ch_layout,
            ffi::AV_SAMPLE_FMT_FLT,
            out_sample_rate,
            in_ch_layout,
            frame.format,
            in_sample_rate,
        ) {
            Ok(ctx) => ctx,
            Err(e) => {
                warn!("Failed to create audio resampler: {}", e);
                return;
            }
        };

        let mut out_frame = AVFrame::new();
        out_frame.set_format(ffi::AV_SAMPLE_FMT_FLT);
        out_frame.set_sample_rate(out_sample_rate);
        out_frame.set_ch_layout(*out_ch_layout);
        out_frame.set_nb_samples(nb_samples);

        if let Err(e) = out_frame.alloc_buffer() {
            warn!("Failed to allocate output audio buffer: {}", e);
            return;
        }

        if let Err(e) = swr_ctx.convert_frame(Some(frame), &mut out_frame) {
            warn!("Failed to convert audio samples: {}", e);
            return;
        }

        let channels = out_ch_layout.nb_channels as usize;
        let sample_count = out_frame.nb_samples as usize;

        unsafe {
            let data_ptr = out_frame.data[0] as *const f32;
            let slice = std::slice::from_raw_parts(data_ptr, sample_count * channels);
            samples.extend_from_slice(slice);
        }
    }

    fn scaled_dimensions(width: u32, height: u32) -> (u32, u32) {
        if width <= MAX_VIDEO_WIDTH {
            return (width.max(1), height.max(1));
        }

        let scale = MAX_VIDEO_WIDTH as f32 / width as f32;
        let new_width = MAX_VIDEO_WIDTH;
        let new_height = (height as f32 * scale).round() as u32;
        (new_width.max(1), new_height.max(1))
    }

    fn fps_from_stream(stream: &rsmpeg::avformat::AVStream) -> f64 {
        let avg = stream.avg_frame_rate;
        rational_to_f64(avg.num, avg.den).unwrap_or_else(|| {
            let rate = stream.r_frame_rate;
            rational_to_f64(rate.num, rate.den).unwrap_or(24.0)
        })
    }

    fn rational_to_f64(num: i32, den: i32) -> Option<f64> {
        if den == 0 {
            None
        } else {
            Some(num as f64 / den as f64)
        }
    }
}

#[cfg(any(target_arch = "wasm32", target_os = "android"))]
mod imp {
    use crate::Error;
    use egui::{Context, TextureHandle};
    use std::path::PathBuf;
    use std::time::Duration;

    #[derive(Debug, Clone, Copy)]
    pub struct VideoClipMeta {
        pub width: u32,
        pub height: u32,
        pub duration: Duration,
        pub frame_interval: Duration,
        pub frame_count: usize,
    }

    #[derive(Debug, Clone)]
    pub enum VideoClipState {
        Unsupported,
        NotLoaded,
        Queued,
        Loading,
        Ready(VideoClipMeta),
        Error(String),
    }

    #[derive(Debug, Clone, Default)]
    pub struct VideoPlaybackState {
        playing: bool,
        current_frame: usize,
    }

    impl VideoPlaybackState {
        pub fn update(&mut self, _now: f64, _meta: &VideoClipMeta) {}
        pub fn toggle(&mut self, _now: f64) {
            self.playing = !self.playing;
        }
        pub fn set_playing(&mut self, playing: bool, _now: f64) {
            self.playing = playing;
        }
        pub fn seek_seconds(&mut self, _seconds: f32, _meta: &VideoClipMeta) {
            self.current_frame = 0;
        }
        pub fn current_frame(&self, _total: usize) -> usize {
            self.current_frame
        }
        pub fn is_playing(&self) -> bool {
            self.playing
        }
        pub fn current_time(&self, _meta: &VideoClipMeta) -> f32 {
            0.0
        }
    }

    pub struct VideoStore {}

    impl VideoStore {
        pub fn new(_cache_dir: PathBuf) -> Self {
            Self {}
        }

        pub fn clip_state(&mut self, _url: &str) -> VideoClipState {
            VideoClipState::Unsupported
        }

        pub fn frame_texture(
            &mut self,
            _ctx: &Context,
            _url: &str,
            _index: usize,
        ) -> Option<TextureHandle> {
            None
        }

        pub fn meta(&self, _url: &str) -> Option<VideoClipMeta> {
            None
        }

        pub fn playback_mut(&mut self, _url: &str) -> &mut VideoPlaybackState {
            static mut DUMMY_STATE: VideoPlaybackState = VideoPlaybackState {
                playing: false,
                current_frame: 0,
            };
            unsafe { &mut DUMMY_STATE }
        }

        pub fn is_audio_active(&self, _url: &str) -> bool {
            false
        }

        pub fn play_audio_from(&mut self, _url: &str, _start_secs: f32) {}

        pub fn stop_audio(&mut self, _url: &str) {}

        pub fn clear_cache(&mut self) -> std::io::Result<()> {
            Ok(())
        }

        pub fn preload_upcoming_textures(
            &mut self,
            _ctx: &Context,
            _url: &str,
            _current_frame: usize,
        ) {
        }

        pub fn request_video_load(&mut self, _url: &str) {}

        pub fn request_full_video(&mut self, _url: &str) {}

        pub fn should_auto_play(&mut self, _url: &str) -> bool {
            false
        }

        pub fn thumbnail_texture(&mut self, _ctx: &Context, _url: &str) -> Option<TextureHandle> {
            None
        }
    }
}

pub use imp::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_video_clip_meta_aspect_ratio() {
        let meta = VideoClipMeta {
            width: 1920,
            height: 1080,
            duration: Duration::from_secs(10),
            frame_interval: Duration::from_millis(33),
            frame_count: 300,
        };

        assert_eq!(meta.aspect_ratio(), 1920.0 / 1080.0);
        assert!((meta.aspect_ratio() - 16.0 / 9.0).abs() < 0.01);
    }

    #[test]
    fn test_video_clip_meta_aspect_ratio_zero_height() {
        let meta = VideoClipMeta {
            width: 1920,
            height: 0,
            duration: Duration::from_secs(10),
            frame_interval: Duration::from_millis(33),
            frame_count: 300,
        };

        assert_eq!(meta.aspect_ratio(), 1.0);
    }

    #[test]
    fn test_video_clip_meta_duration_secs() {
        let meta = VideoClipMeta {
            width: 640,
            height: 480,
            duration: Duration::from_millis(5500),
            frame_interval: Duration::from_millis(33),
            frame_count: 166,
        };

        assert_eq!(meta.duration_secs(), 5.5);
    }

    #[test]
    fn test_video_clip_meta_frame_interval_secs() {
        let meta = VideoClipMeta {
            width: 640,
            height: 480,
            duration: Duration::from_secs(10),
            frame_interval: Duration::from_millis(33),
            frame_count: 303,
        };

        assert!((meta.frame_interval_secs() - 0.033).abs() < 0.001);
    }

    #[test]
    fn test_playback_state_defaults() {
        let state = VideoPlaybackState::default();
        assert!(!state.is_playing());
        assert_eq!(state.current_frame(100), 0);
    }

    #[test]
    fn test_playback_toggle() {
        let mut state = VideoPlaybackState::default();
        let now = 0.0;

        state.toggle(now);
        assert!(state.is_playing());

        state.toggle(now);
        assert!(!state.is_playing());
    }

    #[test]
    fn test_playback_set_playing() {
        let mut state = VideoPlaybackState::default();
        let now = 0.0;

        state.set_playing(true, now);
        assert!(state.is_playing());

        state.set_playing(false, now);
        assert!(!state.is_playing());
    }

    #[test]
    fn test_playback_seek_seconds() {
        let meta = VideoClipMeta {
            width: 640,
            height: 480,
            duration: Duration::from_secs(10),
            frame_interval: Duration::from_millis(100), // 10 fps
            frame_count: 100,
        };

        let mut state = VideoPlaybackState::default();

        // Seek to 5 seconds (should be frame 50 at 10fps with 100ms interval)
        state.seek_seconds(5.0, &meta);
        assert_eq!(state.current_frame(meta.frame_count), 50);

        // Seek beyond duration (should clamp)
        state.seek_seconds(15.0, &meta);
        assert_eq!(state.current_frame(meta.frame_count), 99); // Last frame (frame_count - 1)

        // Seek to negative (should clamp to 0)
        state.seek_seconds(-1.0, &meta);
        assert_eq!(state.current_frame(meta.frame_count), 0);
    }

    #[test]
    fn test_playback_update_not_playing() {
        let meta = VideoClipMeta {
            width: 640,
            height: 480,
            duration: Duration::from_secs(10),
            frame_interval: Duration::from_millis(100),
            frame_count: 100,
        };

        let mut state = VideoPlaybackState::default();
        // Seek to frame 10
        state.seek_seconds(1.0, &meta); // 1 second at 10fps = frame 10

        state.update(1.0, &meta);

        // Frame should not advance when not playing
        assert_eq!(state.current_frame(meta.frame_count), 10);
    }

    #[test]
    fn test_playback_update_advances_frames() {
        let meta = VideoClipMeta {
            width: 640,
            height: 480,
            duration: Duration::from_secs(10),
            frame_interval: Duration::from_millis(100), // 10 fps
            frame_count: 100,
        };

        let mut state = VideoPlaybackState::default();
        let start_frame = state.current_frame(meta.frame_count);
        state.set_playing(true, 0.0);

        // Advance time - frames should advance
        state.update(1.0, &meta);
        let advanced_frame = state.current_frame(meta.frame_count);

        // Verify frames advanced (should be ~10 frames at 10fps over 1 second)
        assert!(advanced_frame > start_frame, "Frames should advance when playing");
        assert!(advanced_frame <= 10, "Should not advance too many frames");
    }

    #[test]
    fn test_playback_resets_when_past_end() {
        let meta = VideoClipMeta {
            width: 640,
            height: 480,
            duration: Duration::from_secs(1),
            frame_interval: Duration::from_millis(100),
            frame_count: 10,
        };

        let mut state = VideoPlaybackState::default();
        state.set_playing(true, 0.0);

        // Play way past the end
        state.update(100.0, &meta);

        // Frame should reset to 0 when going past end (non-looping behavior)
        assert_eq!(state.current_frame(meta.frame_count), 0, "Frame should reset to 0 after end");
    }

    #[test]
    fn test_playback_from_start_to_end() {
        let meta = VideoClipMeta {
            width: 640,
            height: 480,
            duration: Duration::from_secs(1),
            frame_interval: Duration::from_millis(100),
            frame_count: 10,
        };

        let mut state = VideoPlaybackState::default();
        state.set_playing(true, 0.0);

        // Start at frame 0
        assert_eq!(state.current_frame(meta.frame_count), 0);

        // Play through entire duration
        state.update(1.5, &meta); // Beyond end

        // Should stop playing when reaching the end
        assert!(!state.is_playing(), "Should stop playing at end");
    }

    #[test]
    fn test_current_frame_bounds_checking() {
        let meta = VideoClipMeta {
            width: 640,
            height: 480,
            duration: Duration::from_secs(10),
            frame_interval: Duration::from_millis(100),
            frame_count: 100,
        };

        let mut state = VideoPlaybackState::default();
        // Seek beyond the end to test clamping
        state.seek_seconds(100.0, &meta); // Way beyond 10 second duration

        // current_frame() should clamp to valid range
        assert_eq!(state.current_frame(meta.frame_count), 99);
    }

    #[test]
    fn test_current_time() {
        let meta = VideoClipMeta {
            width: 640,
            height: 480,
            duration: Duration::from_secs(10),
            frame_interval: Duration::from_millis(100),
            frame_count: 100,
        };

        let mut state = VideoPlaybackState::default();
        state.seek_seconds(5.0, &meta); // Seek to 5 seconds (frame 50)

        let current_time = state.current_time(&meta);
        assert!((current_time - 5.0).abs() < 0.01); // Frame 50 at 10fps = 5 seconds
    }
}
