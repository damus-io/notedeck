#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
mod imp {
    use crate::imgcache::MediaCache;
    use crate::media::load_texture_checked;
    use crate::{Error, Result};
    use egui::{ColorImage, Context, TextureHandle};
    use ffmpeg_next as ffmpeg;
    use ffmpeg_next::codec;
    use ffmpeg_next::media;
    use ffmpeg_next::software::{resampling, scaling};
    use ffmpeg_next::{format, frame, util};
    use poll_promise::Promise;
    use rodio::{OutputStream, OutputStreamHandle, Sink, Source};
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::io::{Read, Write};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, OnceLock};
    use std::time::Duration;
    use tracing::warn;
    use ureq;

    const MAX_VIDEO_BYTES: usize = 150 * 1024 * 1024;
    const MAX_VIDEO_FRAMES: usize = 600;
    const MAX_VIDEO_WIDTH: u32 = 480;
    const MAX_TARGET_FPS: f64 = 18.0;

    static FFMPEG_INIT: OnceLock<()> = OnceLock::new();

    fn ensure_ffmpeg_initialized() {
        FFMPEG_INIT.get_or_init(|| {
            let _ = ffmpeg::init();
        });
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
        Loading,
        ThumbnailReady(VideoClipMeta),
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
            if frame.texture.is_none() {
                let tex = load_texture_checked(
                    ctx,
                    format!("video:{}:{index}", self.key.clone()),
                    frame.image.clone(),
                    Default::default(),
                );
                frame.texture = Some(tex);
            }
            frame.texture.as_ref()
        }

        fn preload_textures(&mut self, ctx: &Context, start_index: usize, count: usize) {
            let end_index = (start_index + count).min(self.frames.len());
            for index in start_index..end_index {
                if let Some(frame) = self.frames.get_mut(index) {
                    if frame.texture.is_none() {
                        let tex = load_texture_checked(
                            ctx,
                            format!("video:{}:{index}", self.key.clone()),
                            frame.image.clone(),
                            Default::default(),
                        );
                        frame.texture = Some(tex);
                    }
                }
            }
        }
    }

    enum VideoEntry {
        PendingThumbnail(Promise<Option<Result<VideoThumbnail>>>),
        ThumbnailReady(VideoThumbnail),
        PendingFull(Promise<Option<Result<VideoClip>>>),
        Ready(VideoClip),
        Error(String),
    }

    struct VideoThumbnail {
        key: String,
        thumbnail: VideoFrame,
        meta: VideoClipMeta,
    }

    impl VideoThumbnail {
        fn thumbnail_texture(&mut self, ctx: &Context) -> &TextureHandle {
            if self.thumbnail.texture.is_none() {
                let tex = load_texture_checked(
                    ctx,
                    format!("video:{}:thumb", self.key),
                    self.thumbnail.image.clone(),
                    Default::default(),
                );
                self.thumbnail.texture = Some(tex);
            }
            self.thumbnail.texture.as_ref().unwrap()
        }
    }

    struct AudioDecodeContext {
        index: usize,
        decoder: codec::decoder::Audio,
        resampler: resampling::Context,
        rate: u32,
        channels: u16,
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
            }
        }

        pub fn clip_state(&mut self, url: &str) -> VideoClipState {
            let entry = self.entries.entry(url.to_owned()).or_insert_with(|| {
                Self::spawn_thumbnail_loader(url.to_owned(), self.cache_dir.clone())
            });

            Self::drive_entry(entry);

            match entry {
                VideoEntry::PendingThumbnail(_) | VideoEntry::PendingFull(_) => {
                    VideoClipState::Loading
                }
                VideoEntry::ThumbnailReady(thumb) => VideoClipState::ThumbnailReady(thumb.meta),
                VideoEntry::Ready(clip) => VideoClipState::Ready(clip.meta),
                VideoEntry::Error(err) => VideoClipState::Error(err.clone()),
            }
        }

        pub fn thumbnail_texture(&mut self, ctx: &Context, url: &str) -> Option<TextureHandle> {
            let entry = self.entries.get_mut(url)?;
            match entry {
                VideoEntry::ThumbnailReady(thumb) => Some(thumb.thumbnail_texture(ctx).clone()),
                VideoEntry::Ready(clip) => clip.frame_texture(ctx, 0).cloned(),
                _ => None,
            }
        }

        pub fn request_full_video(&mut self, url: &str) {
            let entry = self.entries.get_mut(url);
            if let Some(VideoEntry::ThumbnailReady(_)) = entry {
                *entry.unwrap() = Self::spawn_full_loader(url.to_owned(), self.cache_dir.clone());
                // Mark this video to auto-play once loaded
                self.auto_play_pending.insert(url.to_owned());
            }
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

        pub fn clear_cache(&mut self) -> std::io::Result<()> {
            self.entries.clear();
            self.playback.clear();
            self.auto_play_pending.clear();
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

        fn spawn_thumbnail_loader(url: String, cache_dir: PathBuf) -> VideoEntry {
            let promise = Promise::spawn_thread("video-thumb-loader", move || {
                ensure_ffmpeg_initialized();
                Some(load_or_decode_thumbnail(&url, &cache_dir))
            });
            VideoEntry::PendingThumbnail(promise)
        }

        fn spawn_full_loader(url: String, cache_dir: PathBuf) -> VideoEntry {
            let promise = Promise::spawn_thread("video-full-loader", move || {
                ensure_ffmpeg_initialized();
                Some(load_or_decode_video(&url, &cache_dir))
            });
            VideoEntry::PendingFull(promise)
        }

        fn drive_entry(entry: &mut VideoEntry) {
            match entry {
                VideoEntry::PendingThumbnail(promise) => {
                    let Some(res) = promise.ready_mut() else {
                        return;
                    };

                    let Some(outcome) = res.take() else {
                        *entry = VideoEntry::Error("Thumbnail promise already taken".into());
                        return;
                    };

                    match outcome {
                        Ok(thumb) => *entry = VideoEntry::ThumbnailReady(thumb),
                        Err(err) => *entry = VideoEntry::Error(err.to_string()),
                    }
                }
                VideoEntry::PendingFull(promise) => {
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
                _ => {}
            }
        }
    }

    fn load_or_decode_thumbnail(url: &str, cache_dir: &Path) -> Result<VideoThumbnail> {
        let key = MediaCache::key(url);
        let path = cache_dir.join(&key);

        if !path.exists() {
            download_video(url, &path)?;
        }

        let result = decode_thumbnail(&path, &key);

        if let Err(e) = &result {
            tracing::error!("Thumbnail decode failed for {}: {}", url, e);
        }

        result
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

    fn decode_thumbnail(path: &Path, key: &str) -> Result<VideoThumbnail> {
        let mut ictx = format::input(&path).map_err(map_ffmpeg_err)?;
        let stream = ictx
            .streams()
            .best(media::Type::Video)
            .ok_or_else(|| Error::Generic("No video stream found".into()))?;

        let video_stream_index = stream.index();
        let context = codec::context::Context::from_parameters(stream.parameters())
            .map_err(map_ffmpeg_err)?;
        let mut decoder = context.decoder().video().map_err(map_ffmpeg_err)?;

        let rotation = get_rotation_from_stream(&stream);

        let (src_width, src_height) = if rotation == 90 || rotation == 270 {
            (decoder.height(), decoder.width())
        } else {
            (decoder.width(), decoder.height())
        };

        let (dst_width, dst_height) = scaled_dimensions(src_width, src_height);

        let mut scaler = scaling::Context::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            ffmpeg::format::Pixel::RGBA,
            dst_width,
            dst_height,
            scaling::flag::Flags::BILINEAR,
        )
        .map_err(map_ffmpeg_err)?;

        let src_fps = fps_from_stream(&stream).max(1.0);
        let stride = if src_fps > MAX_TARGET_FPS {
            (src_fps / MAX_TARGET_FPS).round().max(1.0) as usize
        } else {
            1
        };
        let effective_fps = (src_fps / stride as f64).max(1.0);
        let frame_interval = Duration::from_secs_f64(1.0 / effective_fps);

        // Get duration from stream metadata
        let duration = if stream.duration() > 0 {
            let time_base = stream.time_base();
            Duration::from_secs_f64(
                stream.duration() as f64 * time_base.numerator() as f64
                    / time_base.denominator() as f64,
            )
        } else {
            // Fallback: estimate from container duration (duration is in AV_TIME_BASE units)
            Duration::from_micros(ictx.duration() as u64)
        };

        let frame_count = ((duration.as_secs_f64() / frame_interval.as_secs_f64()).ceil() as usize)
            .min(MAX_VIDEO_FRAMES);

        let meta = VideoClipMeta {
            width: dst_width,
            height: dst_height,
            duration,
            frame_interval,
            frame_count,
        };

        // Extract just the first frame
        let mut decoded = frame::Video::empty();
        let mut rgb = frame::Video::empty();
        let mut thumbnail_image = None;

        for (stream, packet) in ictx.packets() {
            if stream.index() != video_stream_index {
                continue;
            }

            decoder.send_packet(&packet).map_err(map_ffmpeg_err)?;
            if decoder.receive_frame(&mut decoded).is_ok() {
                scaler.run(&decoded, &mut rgb).map_err(map_ffmpeg_err)?;
                let mut image = frame_to_image(&rgb);

                image = match rotation {
                    90 => rotate_image_90_cw(image),
                    180 => rotate_image_180(image),
                    270 => rotate_image_270_cw(image),
                    _ => image,
                };

                thumbnail_image = Some(image);
                break;
            }
        }

        let thumbnail_image = thumbnail_image
            .ok_or_else(|| Error::Generic("Unable to decode first frame from video".into()))?;

        Ok(VideoThumbnail {
            key: key.to_owned(),
            thumbnail: VideoFrame {
                image: thumbnail_image,
                texture: None,
            },
            meta,
        })
    }

    fn decode_video(path: &Path, key: &str) -> Result<VideoClip> {
        let mut ictx = format::input(&path).map_err(map_ffmpeg_err)?;
        let stream = ictx
            .streams()
            .best(media::Type::Video)
            .ok_or_else(|| Error::Generic("No video stream found".into()))?;

        let video_stream_index = stream.index();
        let context = codec::context::Context::from_parameters(stream.parameters())
            .map_err(map_ffmpeg_err)?;
        let mut decoder = context.decoder().video().map_err(map_ffmpeg_err)?;

        let rotation = get_rotation_from_stream(&stream);

        let (src_width, src_height) = if rotation == 90 || rotation == 270 {
            (decoder.height(), decoder.width())
        } else {
            (decoder.width(), decoder.height())
        };

        let (dst_width, dst_height) = scaled_dimensions(src_width, src_height);

        let mut scaler = scaling::Context::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            ffmpeg::format::Pixel::RGBA,
            dst_width,
            dst_height,
            scaling::flag::Flags::BILINEAR,
        )
        .map_err(map_ffmpeg_err)?;

        let src_fps = fps_from_stream(&stream).max(1.0);
        let stride = if src_fps > MAX_TARGET_FPS {
            (src_fps / MAX_TARGET_FPS).round().max(1.0) as usize
        } else {
            1
        };
        let effective_fps = (src_fps / stride as f64).max(1.0);
        let frame_interval = Duration::from_secs_f64(1.0 / effective_fps);

        let mut audio_ctx = ictx
            .streams()
            .find(|s| s.parameters().medium() == media::Type::Audio)
            .and_then(|stream| {
                let index = stream.index();
                let context = codec::context::Context::from_parameters(stream.parameters()).ok()?;
                let decoder = context.decoder().audio().ok()?;
                let channel_layout = decoder.channel_layout();
                let rate = decoder.rate();
                let resampler = resampling::Context::get(
                    decoder.format(),
                    channel_layout,
                    rate,
                    util::format::Sample::F32(util::format::sample::Type::Packed),
                    channel_layout,
                    rate,
                )
                .ok()?;
                Some(AudioDecodeContext {
                    index,
                    decoder,
                    resampler,
                    rate,
                    channels: channel_layout.channels() as u16,
                })
            });
        let mut audio_samples: Vec<f32> = Vec::new();
        let mut audio_frame = frame::Audio::empty();
        let mut converted_audio = frame::Audio::empty();

        let mut frames = Vec::new();
        let mut decoded = frame::Video::empty();
        let mut rgb = frame::Video::empty();
        let mut frame_index = 0usize;
        let mut video_done = false;

        for (stream, packet) in ictx.packets() {
            let stream_index = stream.index();

            if stream_index == video_stream_index && !video_done {
                decoder.send_packet(&packet).map_err(map_ffmpeg_err)?;
                while decoder.receive_frame(&mut decoded).is_ok() {
                    if frame_index % stride == 0 {
                        scaler.run(&decoded, &mut rgb).map_err(map_ffmpeg_err)?;
                        let mut image = frame_to_image(&rgb);

                        image = match rotation {
                            90 => rotate_image_90_cw(image),
                            180 => rotate_image_180(image),
                            270 => rotate_image_270_cw(image),
                            _ => image,
                        };

                        frames.push(image);
                        if frames.len() >= MAX_VIDEO_FRAMES {
                            video_done = true;
                            break;
                        }
                    }
                    frame_index += 1;
                }
            } else if let Some(ctx) = audio_ctx.as_mut() {
                if stream_index == ctx.index {
                    ctx.decoder.send_packet(&packet).map_err(map_ffmpeg_err)?;
                    while ctx.decoder.receive_frame(&mut audio_frame).is_ok() {
                        ctx.resampler
                            .run(&audio_frame, &mut converted_audio)
                            .map_err(map_ffmpeg_err)?;
                        let sample_count = converted_audio.samples();
                        if sample_count == 0 {
                            continue;
                        }

                        let total_samples_needed = sample_count * ctx.channels as usize;
                        let data_ptr = converted_audio.data(0).as_ptr() as *const f32;
                        let samples_slice =
                            unsafe { std::slice::from_raw_parts(data_ptr, total_samples_needed) };

                        audio_samples.extend_from_slice(samples_slice);
                    }
                }
            }
        }

        decoder.send_eof().ok();
        while decoder.receive_frame(&mut decoded).is_ok() {
            if frame_index % stride == 0 {
                scaler.run(&decoded, &mut rgb).map_err(map_ffmpeg_err)?;
                let mut image = frame_to_image(&rgb);

                image = match rotation {
                    90 => rotate_image_90_cw(image),
                    180 => rotate_image_180(image),
                    270 => rotate_image_270_cw(image),
                    _ => image,
                };

                frames.push(image);
                if frames.len() >= MAX_VIDEO_FRAMES {
                    break;
                }
            }
            frame_index += 1;
        }

        if let Some(ctx) = audio_ctx.as_mut() {
            ctx.decoder.send_eof().ok();
            while ctx.decoder.receive_frame(&mut audio_frame).is_ok() {
                ctx.resampler
                    .run(&audio_frame, &mut converted_audio)
                    .map_err(map_ffmpeg_err)?;
                let sample_count = converted_audio.samples();
                if sample_count == 0 {
                    continue;
                }
                let total_samples_needed = sample_count * ctx.channels as usize;
                let data_ptr = converted_audio.data(0).as_ptr() as *const f32;
                let samples_slice =
                    unsafe { std::slice::from_raw_parts(data_ptr, total_samples_needed) };
                audio_samples.extend_from_slice(samples_slice);
            }

            while let Ok(Some(_)) = ctx.resampler.flush(&mut converted_audio) {
                let sample_count = converted_audio.samples();
                if sample_count == 0 {
                    break;
                }
                let total_samples_needed = sample_count * ctx.channels as usize;
                let data_ptr = converted_audio.data(0).as_ptr() as *const f32;
                let samples_slice =
                    unsafe { std::slice::from_raw_parts(data_ptr, total_samples_needed) };
                audio_samples.extend_from_slice(samples_slice);
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

        let audio = if let Some(ctx) = audio_ctx {
            if audio_samples.is_empty() {
                None
            } else {
                Some(AudioClip::new(audio_samples, ctx.rate, ctx.channels))
            }
        } else {
            None
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

    fn get_rotation_from_stream(stream: &ffmpeg::Stream) -> i32 {
        if let Some(rotation_str) = stream.metadata().get("rotate") {
            if let Ok(rotation) = rotation_str.parse::<i32>() {
                return ((rotation % 360) + 360) % 360;
            }
        }
        0
    }

    fn rotate_image_90_cw(image: ColorImage) -> ColorImage {
        let ColorImage {
            size: [width, height],
            pixels,
        } = image;
        let mut rotated = vec![egui::Color32::BLACK; width * height];

        for y in 0..height {
            for x in 0..width {
                let src_idx = y * width + x;
                let dst_x = y;
                let dst_y = width - 1 - x;
                let dst_idx = dst_y * height + dst_x;
                rotated[dst_idx] = pixels[src_idx];
            }
        }

        ColorImage {
            size: [height, width],
            pixels: rotated,
        }
    }

    fn rotate_image_180(image: ColorImage) -> ColorImage {
        let ColorImage { size, mut pixels } = image;
        pixels.reverse();
        ColorImage { size, pixels }
    }

    fn rotate_image_270_cw(image: ColorImage) -> ColorImage {
        let ColorImage {
            size: [width, height],
            pixels,
        } = image;
        let mut rotated = vec![egui::Color32::BLACK; width * height];

        for y in 0..height {
            for x in 0..width {
                let src_idx = y * width + x;
                let dst_x = height - 1 - y;
                let dst_y = x;
                let dst_idx = dst_y * height + dst_x;
                rotated[dst_idx] = pixels[src_idx];
            }
        }

        ColorImage {
            size: [height, width],
            pixels: rotated,
        }
    }

    fn frame_to_image(frame: &frame::Video) -> ColorImage {
        let width = frame.width() as usize;
        let height = frame.height() as usize;
        let stride = frame.stride(0);
        let data = frame.data(0);
        let mut pixels = vec![0u8; width * height * 4];

        for y in 0..height {
            let src_offset = y * stride;
            let dst_offset = y * width * 4;
            let row = &data[src_offset..src_offset + width * 4];
            pixels[dst_offset..dst_offset + width * 4].copy_from_slice(row);
        }

        ColorImage::from_rgba_unmultiplied([width, height], &pixels)
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

    fn fps_from_stream(stream: &ffmpeg::Stream) -> f64 {
        let avg = stream.avg_frame_rate();
        rational_to_f64(avg).unwrap_or_else(|| rational_to_f64(stream.rate()).unwrap_or(24.0))
    }

    fn rational_to_f64(r: util::rational::Rational) -> Option<f64> {
        let num = r.numerator();
        let denom = r.denominator();
        if denom == 0 {
            None
        } else {
            Some(num as f64 / denom as f64)
        }
    }

    fn map_ffmpeg_err(err: ffmpeg::Error) -> Error {
        Error::Generic(format!("ffmpeg error: {err}"))
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
        Loading,
        ThumbnailReady(VideoClipMeta),
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

    pub struct VideoStore {
        // Stub implementation for platforms that don't support inline video
        // No cache directory needed since videos are never loaded
    }

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
            // Return a static dummy state since we don't actually play videos on this platform
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
            // No-op on unsupported platforms
        }
    }
}

pub use imp::*;
