#![warn(missing_docs)]
//! egui-video
//! video playback library for [`egui`]
//!
extern crate ffmpeg_next as ffmpeg;
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use egui::{
    vec2, Align2, Color32, ColorImage, CornerRadius, FontId, Image, Rect, Response, Sense,
    TextureHandle, TextureOptions, Ui, Vec2,
};
use ffmpeg::ffi::AV_TIME_BASE;
use ffmpeg::format::context::input::Input;
use ffmpeg::format::{input, Pixel};
use ffmpeg::frame::Audio;
use ffmpeg::media::Type;
use ffmpeg::util::frame::video::Video;
use ffmpeg::{rescale, Rational, Rescale};
use ffmpeg::{software, ChannelLayout};
use parking_lot::Mutex;
use ringbuf::SharedRb;
use sdl2::audio::{self, AudioCallback, AudioFormat, AudioSpecDesired};
use std::sync::{Arc, Once};
use std::time::UNIX_EPOCH;
use timer::{Guard, Timer};

#[cfg(feature = "from_bytes")]
use std::io::Write;
#[cfg(feature = "from_bytes")]
use tempfile::NamedTempFile;

fn format_duration(dur: Duration) -> String {
    let dt = DateTime::<Utc>::from(UNIX_EPOCH) + dur;
    if dt.format("%H").to_string().parse::<i64>().unwrap() > 0 {
        dt.format("%H:%M:%S").to_string()
    } else {
        dt.format("%M:%S").to_string()
    }
}

static FFMPEG_INIT: Once = Once::new();

/// Ensure ffmpeg is initialized before creating any players.
pub fn ensure_initialized() {
    FFMPEG_INIT.call_once(|| {
        if let Err(err) = ffmpeg::init() {
            panic!("failed to initialize ffmpeg: {err}");
        }
    });
}

/// The playback device. Needs to be initialized (and kept alive!) for use by a [`Player`].
pub type AudioDevice = audio::AudioDevice<AudioDeviceCallback>;

type AudioSampleProducer =
    ringbuf::Producer<f32, Arc<ringbuf::SharedRb<f32, Vec<std::mem::MaybeUninit<f32>>>>>;
type AudioSampleConsumer =
    ringbuf::Consumer<f32, Arc<ringbuf::SharedRb<f32, Vec<std::mem::MaybeUninit<f32>>>>>;

/// The [`Player`] processes and controls streams of video/audio. This is what you use to show a video file.
/// Initialize once, and use the [`Player::ui`] or [`Player::ui_at()`] functions to show the playback.
pub struct Player {
    /// The video streamer of the player.
    pub video_streamer: Arc<Mutex<VideoStreamer>>,
    /// The audio streamer of the player. Won't exist unless [`Player::with_audio`] is called and there exists
    /// a valid audio stream in the file.
    pub audio_streamer: Option<Arc<Mutex<AudioStreamer>>>,
    /// The state of the player.
    pub player_state: Cache<PlayerState>,
    /// The framerate of the video stream.
    pub framerate: f64,
    texture_options: TextureOptions,
    /// The player's texture handle.
    pub texture_handle: TextureHandle,
    /// The height of the video stream.
    pub height: u32,
    /// The width of the video stream.
    pub width: u32,
    frame_timer: Timer,
    audio_timer: Timer,
    audio_thread: Option<Guard>,
    frame_thread: Option<Guard>,
    ctx_ref: egui::Context,
    /// Should the stream loop if it finishes?
    pub looping: bool,
    /// The volume of the audio stream.
    pub audio_volume: Cache<f32>,
    /// The maximum volume of the audio stream.
    pub max_audio_volume: f32,
    duration_ms: i64,
    last_seek_ms: Option<i64>,
    preseek_player_state: Option<PlayerState>,
    #[cfg(feature = "from_bytes")]
    temp_file: Option<NamedTempFile>,
    video_elapsed_ms: Cache<i64>,
    audio_elapsed_ms: Cache<i64>,
    input_path: String,
}

#[derive(PartialEq, Clone, Copy, Debug)]
/// The possible states of a [`Player`].
pub enum PlayerState {
    /// No playback.
    Stopped,
    /// Streams have reached the end of the file.
    EndOfFile,
    /// User is seeking. Value is from `0` to `1` indicationg target location in the stream.
    Seeking(f32),
    /// Playback is paused.
    Paused,
    /// Playback is ongoing.
    Playing,
}

/// Streams video.
pub struct VideoStreamer {
    video_decoder: ffmpeg::decoder::Video,
    video_stream_index: usize,
    player_state: Cache<PlayerState>,
    input_context: Input,
    video_elapsed_ms: Cache<i64>,
    _audio_elapsed_ms: Cache<i64>,
    scaler: software::scaling::Context,
}

/// Streams audio.
pub struct AudioStreamer {
    _video_elapsed_ms: Cache<i64>,
    audio_elapsed_ms: Cache<i64>,
    audio_stream_index: usize,
    audio_decoder: ffmpeg::decoder::Audio,
    resampler: software::resampling::Context,
    audio_sample_producer: AudioSampleProducer,
    input_context: Input,
    player_state: Cache<PlayerState>,
}

#[derive(Clone)]
/// Just `Arc<Mutex<T>>` with a local cache.
pub struct Cache<T: Copy> {
    cached_value: T,
    override_value: Option<T>,
    raw_value: Arc<Mutex<T>>,
}

impl<T: Copy> Cache<T> {
    /// Set the value. Blocks the thread until it can aquire the mutex.
    pub fn set(&mut self, value: T) {
        self.cached_value = value;
        *self.raw_value.lock() = value
    }
    /// Get the value.
    /// Priority: Override value -> Try update attempt -> Cached value
    pub fn get(&mut self) -> T {
        self.override_value.unwrap_or(self.get_true())
    }
    /// Get the "true" value, ignoring override.
    /// Priority: Try update attempt -> Cached value
    pub fn get_true(&mut self) -> T {
        self.try_update_cache().unwrap_or(self.cached_value)
    }
    /// Updates the cache. Blocks the thread until it can aquire the mutex.
    pub fn update_cache(&mut self) {
        self.cached_value = *self.raw_value.lock();
    }
    /// Get the updated value. Blocks the thread until it can aquire the mutex.
    pub fn get_updated(&mut self) -> T {
        self.update_cache();
        self.cached_value
    }
    /// Attempt to update the cache by trying to lock the mutex. Returns the updated value as an [`Option`] if it succeeeds.
    pub fn try_update_cache(&mut self) -> Option<T> {
        if let Some(new_value) = self.raw_value.try_lock() {
            self.cached_value = *new_value;
            Some(self.cached_value)
        } else {
            None
        }
    }
    /// Make a new cache.
    pub fn new(value: T) -> Self {
        Self {
            override_value: None,
            cached_value: value,
            raw_value: Arc::new(Mutex::new(value)),
        }
    }
}

const AV_TIME_BASE_RATIONAL: Rational = Rational(1, AV_TIME_BASE);
const MILLISEC_TIME_BASE: Rational = Rational(1, 1000);

fn timestamp_to_millisec(timestamp: i64, time_base: Rational) -> i64 {
    timestamp.rescale(time_base, MILLISEC_TIME_BASE)
}

fn millisec_to_timestamp(millisec: i64, time_base: Rational) -> i64 {
    millisec.rescale(MILLISEC_TIME_BASE, time_base)
}

impl Player {
    /// A formatted string for displaying the duration of the video stream.
    pub fn duration_text(&mut self) -> String {
        format!(
            "{} / {}",
            format_duration(Duration::milliseconds(self.video_elapsed_ms.get())),
            format_duration(Duration::milliseconds(self.duration_ms))
        )
    }
    fn reset(&mut self, start_playing: bool) {
        self.video_streamer.lock().reset(start_playing);
        if let Some(audio_decoder) = self.audio_streamer.as_mut() {
            audio_decoder.lock().reset(start_playing);
        }
    }
    fn set_state(&mut self, new_state: PlayerState) {
        self.player_state.set(new_state)
    }
    /// Pause the stream.
    pub fn pause(&mut self) {
        self.set_state(PlayerState::Paused)
    }
    /// Unpause the stream.
    pub fn unpause(&mut self) {
        self.set_state(PlayerState::Playing)
    }
    /// Stop the stream.
    pub fn stop(&mut self) {
        self.set_state(PlayerState::Stopped)
    }
    fn duration_frac(&mut self) -> f32 {
        self.video_elapsed_ms.get() as f32 / self.duration_ms as f32
    }
    fn spawn_timers(&mut self) {
        let mut texture_handle = self.texture_handle.clone();
        let texture_options = self.texture_options.clone();
        let ctx = self.ctx_ref.clone();
        let stream_decoder = Arc::clone(&self.video_streamer);
        let wait_duration = Duration::milliseconds((1000. / self.framerate) as i64);
        let duration_ms = self.duration_ms;
        let frame_timer_guard = self.frame_timer.schedule_repeating(wait_duration, move || {
            ctx.request_repaint();
            stream_decoder
                .lock()
                .process_state(duration_ms, true, |frame| {
                    texture_handle.set(frame, texture_options)
                });
        });
        self.frame_thread = Some(frame_timer_guard);

        if let Some(audio_decoder) = self.audio_streamer.as_ref() {
            let audio_decoder = Arc::clone(&audio_decoder);
            let audio_timer_guard =
                self.audio_timer
                    .schedule_repeating(Duration::zero(), move || {
                        audio_decoder
                            .lock()
                            .process_state(duration_ms, false, |_| {});
                    });
            self.audio_thread = Some(audio_timer_guard);
        }
    }
    /// Start the stream.
    pub fn start(&mut self) {
        self.frame_thread = None;
        self.audio_thread = None;
        self.reset(true);
        self.spawn_timers();
    }

    fn process_state(&mut self) {
        let mut reset_stream = false;
        let video_elapsed_ms = self.video_elapsed_ms.get();
        if self.last_seek_ms.is_some() {
            let last_seek_ms = *self.last_seek_ms.as_ref().unwrap();
            if self.video_elapsed_ms.get_true() > last_seek_ms || video_elapsed_ms == 0 {
                self.video_elapsed_ms.override_value = None;
                self.last_seek_ms = None;
            } else {
                self.video_elapsed_ms.override_value = Some(last_seek_ms);
            }
        } else {
            self.video_elapsed_ms.override_value = None;
        }

        match self.player_state.get_updated() {
            PlayerState::EndOfFile => {
                if self.looping {
                    reset_stream = true;
                } else {
                    self.player_state.set(PlayerState::Stopped);
                }
            }
            PlayerState::Stopped => {
                self.frame_thread = None;
                self.audio_thread = None;
            }
            _ => (),
        }

        if reset_stream {
            self.reset(true);
        }
    }

    /// Draw the player's ui.
    pub fn ui(&mut self, ui: &mut Ui, size: [f32; 2]) -> egui::Response {
        self.process_state();
        let image = Image::new((self.texture_handle.id(), Vec2::new(size[0], size[1])))
            .sense(Sense::click());
        let response = ui.add(image);
        self.render_ui(ui, &response);
        response
    }

    /// Draw the player's ui with a specific rect.
    pub fn ui_at(&mut self, ui: &mut Ui, rect: Rect) -> egui::Response {
        self.process_state();
        let image = Image::new((self.texture_handle.id(), rect.size())).sense(Sense::click());
        let response = ui.put(rect, image);
        self.render_ui(ui, &response);
        response
    }

    fn render_ui(&mut self, ui: &mut Ui, playback_response: &Response) -> Option<Rect> {
        let hovered = ui.rect_contains_pointer(playback_response.rect);
        let currently_seeking = matches!(self.player_state.get(), PlayerState::Seeking(_));
        let is_stopped = matches!(self.player_state.get(), PlayerState::Stopped);
        let is_paused = matches!(self.player_state.get(), PlayerState::Paused);
        let seekbar_anim_frac = ui.ctx().animate_bool_with_time(
            playback_response.id.with("seekbar_anim"),
            hovered || currently_seeking || is_paused || is_stopped,
            0.2,
        );

        if seekbar_anim_frac > 0. {
            let seekbar_width_offset = 20.;
            let fullseekbar_width = playback_response.rect.width() - seekbar_width_offset;

            let seekbar_width = if let PlayerState::Seeking(h) = self.player_state.get() {
                fullseekbar_width * h
            } else {
                fullseekbar_width * self.duration_frac()
            };
            if playback_response.clicked() {
                let mut reset_stream = false;
                let mut start_stream = false;

                match self.player_state.get() {
                    PlayerState::Stopped => start_stream = true,
                    PlayerState::EndOfFile => reset_stream = true,
                    PlayerState::Paused => self.player_state.set(PlayerState::Playing),
                    PlayerState::Playing => self.player_state.set(PlayerState::Paused),
                    _ => (),
                }

                if reset_stream {
                    self.reset(true);
                } else if start_stream {
                    self.start();
                }
            }
            let seekbar_offset = 20.;
            let seekbar_pos = playback_response.rect.left_bottom()
                + vec2(seekbar_width_offset / 2., -seekbar_offset);
            let seekbar_height = 3.;
            let mut fullseekbar_rect =
                Rect::from_min_size(seekbar_pos, vec2(fullseekbar_width, seekbar_height));

            let mut seekbar_rect =
                Rect::from_min_size(seekbar_pos, vec2(seekbar_width, seekbar_height));
            let seekbar_interact_rect = fullseekbar_rect.expand(10.);
            ui.interact(seekbar_interact_rect, playback_response.id, Sense::drag());

            let seekbar_hovered = ui.rect_contains_pointer(seekbar_interact_rect);
            let seekbar_hover_anim_frac = ui.ctx().animate_bool_with_time(
                playback_response.id.with("seekbar_hover_anim"),
                seekbar_hovered || currently_seeking,
                0.2,
            );

            if seekbar_hover_anim_frac > 0. {
                let new_top = fullseekbar_rect.top() - (3. * seekbar_hover_anim_frac);
                fullseekbar_rect.set_top(new_top);
                seekbar_rect.set_top(new_top);
            }

            if seekbar_hovered || currently_seeking {
                if let Some(hover_pos) = ui.ctx().input(|i| i.pointer.hover_pos()) {
                    let seek_frac = ((hover_pos - playback_response.rect.left_top()).x
                        - seekbar_width_offset / 2.)
                        .max(0.)
                        .min(fullseekbar_width)
                        / fullseekbar_width;
                    if ui.ctx().input(|i| i.pointer.primary_down()) {
                        if is_stopped {
                            self.reset(true);
                            self.spawn_timers();
                        }
                        if !currently_seeking {
                            self.preseek_player_state = Some(self.player_state.get_updated());
                        }
                        self.set_state(PlayerState::Seeking(seek_frac));
                        self.last_seek_ms =
                            Some((seek_frac as f64 * self.duration_ms as f64) as i64);
                        seekbar_rect.set_right(
                            hover_pos
                                .x
                                .min(fullseekbar_rect.right())
                                .max(fullseekbar_rect.left()),
                        );
                    } else if ui.ctx().input(|i| i.pointer.any_released()) {
                        if let Some(previous_state) = self.preseek_player_state.take() {
                            self.set_state(previous_state)
                        } else {
                            self.set_state(PlayerState::Playing)
                        }
                    }
                }
            }
            let text_color = Color32::WHITE.linear_multiply(seekbar_anim_frac);

            let pause_icon = if is_paused {
                "▶"
            } else if is_stopped {
                "◼"
            } else if currently_seeking {
                "↔"
            } else {
                "⏸"
            };
            let audio_volume_frac = self.audio_volume.get() / self.max_audio_volume;
            let sound_icon = if audio_volume_frac > 0.7 {
                "🔊"
            } else if audio_volume_frac > 0.4 {
                "🔉"
            } else if audio_volume_frac > 0. {
                "🔈"
            } else {
                "🔇"
            };
            let mut icon_font_id = FontId::default();
            icon_font_id.size = 16.;

            let text_y_offset = -7.;
            let sound_icon_offset = vec2(-5., text_y_offset);
            let sound_icon_pos = fullseekbar_rect.right_top() + sound_icon_offset;

            let pause_icon_offset = vec2(3., text_y_offset);
            let pause_icon_pos = fullseekbar_rect.left_top() + pause_icon_offset;

            let duration_text_offset = vec2(25., text_y_offset);
            let duration_text_pos = fullseekbar_rect.left_top() + duration_text_offset;
            let mut duration_text_font_id = FontId::default();
            duration_text_font_id.size = 14.;

            let fullseekbar_color = Color32::GRAY.linear_multiply(seekbar_anim_frac);
            let seekbar_color = Color32::WHITE.linear_multiply(seekbar_anim_frac);

            ui.painter().rect_filled(
                fullseekbar_rect,
                CornerRadius::ZERO,
                fullseekbar_color.linear_multiply(0.5),
            );
            ui.painter()
                .rect_filled(seekbar_rect, CornerRadius::ZERO, seekbar_color);
            ui.painter().text(
                pause_icon_pos,
                Align2::LEFT_BOTTOM,
                pause_icon,
                icon_font_id.clone(),
                text_color,
            );

            ui.painter().text(
                duration_text_pos,
                Align2::LEFT_BOTTOM,
                self.duration_text(),
                duration_text_font_id,
                text_color,
            );

            if seekbar_hover_anim_frac > 0. {
                ui.painter().circle_filled(
                    seekbar_rect.right_center(),
                    7. * seekbar_hover_anim_frac,
                    seekbar_color,
                );
            }

            if self.audio_streamer.is_some() {
                let sound_icon_rect = ui.painter().text(
                    sound_icon_pos,
                    Align2::RIGHT_BOTTOM,
                    sound_icon,
                    icon_font_id.clone(),
                    text_color,
                );

                if ui
                    .interact(
                        sound_icon_rect,
                        playback_response.id.with("sound_icon_sense"),
                        Sense::click(),
                    )
                    .clicked()
                {
                    if self.audio_volume.get() != 0. {
                        self.audio_volume.set(0.)
                    } else {
                        self.audio_volume.set(self.max_audio_volume / 2.)
                    }
                }

                let sound_slider_outer_height = 75.;
                let sound_slider_margin = 5.;
                let sound_slider_opacity = 100;
                let mut sound_slider_rect = sound_icon_rect;
                sound_slider_rect.set_bottom(sound_icon_rect.top() - sound_slider_margin);
                sound_slider_rect.set_top(sound_slider_rect.top() - sound_slider_outer_height);

                let sound_slider_interact_rect = sound_slider_rect.expand(sound_slider_margin);
                let sound_hovered = ui.rect_contains_pointer(sound_icon_rect);
                let sound_slider_hovered = ui.rect_contains_pointer(sound_slider_interact_rect);
                let sound_anim_id = playback_response.id.with("sound_anim");
                let mut sound_anim_frac: f32 = ui
                    .ctx()
                    .memory_mut(|m| *m.data.get_temp_mut_or_default(sound_anim_id));
                sound_anim_frac = ui.ctx().animate_bool_with_time(
                    sound_anim_id,
                    sound_hovered || (sound_slider_hovered && sound_anim_frac > 0.),
                    0.2,
                );
                ui.ctx()
                    .memory_mut(|m| m.data.insert_temp(sound_anim_id, sound_anim_frac));
                let sound_slider_bg_color = Color32::from_black_alpha(sound_slider_opacity)
                    .linear_multiply(sound_anim_frac);
                let sound_bar_color = Color32::from_white_alpha(sound_slider_opacity)
                    .linear_multiply(sound_anim_frac);
                let mut sound_bar_rect = sound_slider_rect;
                sound_bar_rect.set_top(
                    sound_bar_rect.bottom()
                        - (self.audio_volume.get() / self.max_audio_volume)
                            * sound_bar_rect.height(),
                );

                ui.painter().rect_filled(
                    sound_slider_rect,
                    CornerRadius::same(5),
                    sound_slider_bg_color,
                );

                ui.painter()
                    .rect_filled(sound_bar_rect, CornerRadius::same(5), sound_bar_color);
                let sound_slider_resp = ui.interact(
                    sound_slider_rect,
                    playback_response.id.with("sound_slider_sense"),
                    Sense::click_and_drag(),
                );
                if sound_anim_frac > 0. && sound_slider_resp.clicked()
                    || sound_slider_resp.dragged()
                {
                    if let Some(hover_pos) = ui.ctx().input(|i| i.pointer.hover_pos()) {
                        let sound_frac = 1.
                            - ((hover_pos - sound_slider_rect.left_top()).y
                                / sound_slider_rect.height())
                            .max(0.)
                            .min(1.);
                        self.audio_volume.set(sound_frac * self.max_audio_volume);
                    }
                }
            }

            Some(seekbar_interact_rect)
        } else {
            None
        }
    }

    #[cfg(feature = "from_bytes")]
    /// Create a new [`Player`] from input bytes.
    pub fn new_from_bytes(ctx: &egui::Context, input_bytes: &[u8]) -> Result<Self> {
        let mut file = tempfile::Builder::new().tempfile()?;
        file.write_all(input_bytes)?;
        let path = file.path().to_string_lossy().to_string();
        let mut slf = Self::new(ctx, &path)?;
        slf.temp_file = Some(file);
        Ok(slf)
    }

    /// Initializes the audio stream (if there is one), required for making a [`Player`] output audio.
    pub fn with_audio(mut self, audio_device: &mut AudioDevice) -> Result<Self> {
        let audio_input_context = input(&self.input_path)?;
        let audio_stream = audio_input_context.streams().best(Type::Audio);

        let audio_streamer = if let Some(audio_stream) = audio_stream.as_ref() {
            let audio_stream_index = audio_stream.index();
            let audio_context =
                ffmpeg::codec::context::Context::from_parameters(audio_stream.parameters())?;
            let audio_decoder = audio_context.decoder().audio()?;
            let audio_sample_buffer =
                SharedRb::<f32, Vec<_>>::new(audio_device.spec().size as usize);
            let (audio_sample_producer, audio_sample_consumer) = audio_sample_buffer.split();
            let audio_resampler = ffmpeg::software::resampling::context::Context::get(
                audio_decoder.format(),
                audio_decoder.channel_layout(),
                audio_decoder.rate(),
                audio_device.spec().format.to_sample(),
                ChannelLayout::STEREO,
                audio_device.spec().freq as u32,
            )?;

            audio_device.lock().sample_streams.push(AudioSampleStream {
                sample_consumer: audio_sample_consumer,
                audio_volume: self.audio_volume.clone(),
            });

            audio_device.resume();
            Some(AudioStreamer {
                player_state: self.player_state.clone(),
                _video_elapsed_ms: self.video_elapsed_ms.clone(),
                audio_elapsed_ms: self.audio_elapsed_ms.clone(),
                audio_sample_producer,
                input_context: audio_input_context,
                audio_decoder,
                audio_stream_index,
                resampler: audio_resampler,
            })
        } else {
            None
        };
        self.audio_streamer = audio_streamer.map(|s| Arc::new(Mutex::new(s)));
        Ok(self)
    }

    /// Create a new [`Player`].
    pub fn new(ctx: &egui::Context, input_path: &String) -> Result<Self> {
        let input_context = input(&input_path)?;
        let video_stream = input_context
            .streams()
            .best(Type::Video)
            .ok_or(ffmpeg::Error::StreamNotFound)?;
        let video_stream_index = video_stream.index();
        let max_audio_volume = 1.;

        let audio_volume = Cache::new(max_audio_volume / 2.);

        let video_elapsed_ms = Cache::new(0);
        let audio_elapsed_ms = Cache::new(0);
        let player_state = Cache::new(PlayerState::Stopped);

        let video_context =
            ffmpeg::codec::context::Context::from_parameters(video_stream.parameters())?;
        let video_decoder = video_context.decoder().video()?;
        let framerate = (video_stream.avg_frame_rate().numerator() as f64)
            / video_stream.avg_frame_rate().denominator() as f64;

        let (width, height) = (video_decoder.width(), video_decoder.height());
        let frame_scaler = software::scaling::Context::get(
            video_decoder.format(),
            video_decoder.width(),
            video_decoder.height(),
            Pixel::RGB24,
            video_decoder.width(),
            video_decoder.height(),
            software::scaling::flag::Flags::BILINEAR,
        )?;

        let duration_ms = timestamp_to_millisec(input_context.duration(), AV_TIME_BASE_RATIONAL); // in sec
        let stream_decoder = VideoStreamer {
            video_decoder,
            video_stream_index,
            _audio_elapsed_ms: audio_elapsed_ms.clone(),
            video_elapsed_ms: video_elapsed_ms.clone(),
            input_context,
            player_state: player_state.clone(),
            scaler: frame_scaler,
        };
        let texture_options = TextureOptions::LINEAR;
        let texture_handle = ctx.load_texture("vidstream", ColorImage::example(), texture_options);
        let mut streamer = Self {
            input_path: input_path.clone(),
            audio_streamer: None,
            video_streamer: Arc::new(Mutex::new(stream_decoder)),
            texture_options,
            framerate,
            frame_timer: Timer::new(),
            audio_timer: Timer::new(),
            preseek_player_state: None,
            frame_thread: None,
            audio_thread: None,
            texture_handle,
            player_state,
            video_elapsed_ms,
            audio_elapsed_ms,
            width,
            last_seek_ms: None,
            duration_ms,
            audio_volume,
            max_audio_volume,
            looping: true,
            height,
            ctx_ref: ctx.clone(),
            #[cfg(feature = "from_bytes")]
            temp_file: None,
        };

        loop {
            if let Ok(_texture_handle) = streamer.try_set_texture_handle() {
                break;
            }
        }

        Ok(streamer)
    }

    fn try_set_texture_handle(&mut self) -> Result<TextureHandle> {
        match self
            .video_streamer
            .lock()
            .recieve_next_packet_until_frame()
            // .and_then(|mut s| Ok(s.))
        {
            Ok(first_frame) => {
                let texture_handle =
                    self.ctx_ref
                        .load_texture("vidstream", first_frame, self.texture_options);
                let texture_handle_clone = texture_handle.clone();
                self.texture_handle = texture_handle;
                Ok(texture_handle_clone)
            }
            Err(e) => Err(e),
        }
    }
}

/// Streams data.
pub trait Streamer {
    /// The associated type of frame used for the stream.
    type Frame;
    /// The associated type after the frame is processed.
    type ProcessedFrame;
    /// Process the streamer's state.
    fn process_state(
        &mut self,
        duration_ms: i64,
        seek_preview: bool,
        apply_processed_frame: impl FnOnce(Self::ProcessedFrame),
    ) {
        let player_state = self.player_state().get();
        if player_state == PlayerState::Playing {
            match self.recieve_next_packet_until_frame() {
                Ok(frame) => {
                    apply_processed_frame(frame);
                }
                Err(_e) => {}
            }
        } else if let PlayerState::Seeking(seek_frac) = player_state {
            let target_ms = (seek_frac as f64 * duration_ms as f64) as i64;
            let seeking_forward = target_ms > self.elapsed_ms().get();
            let target_ts = millisec_to_timestamp(target_ms, rescale::TIME_BASE);
            if let Err(e) = self.input_context().seek(target_ts, ..target_ts) {
                dbg!(e);
            } else {
                if seek_frac >= 0.99 {
                    // prevent inifinite loop near end of stream
                    self.player_state().set(PlayerState::EndOfFile)
                } else if seek_frac > 0. {
                    // this drop frame loop lets us refresh until current_ts is accurate
                    if !seeking_forward {
                        while (self.elapsed_ms().get() as f64 / duration_ms as f64)
                            > seek_frac as f64
                        {
                            self.drop_frames();
                        }
                    }

                    // this drop frame loop drops frames until we are at desired
                    while (self.elapsed_ms().get() as f64 / duration_ms as f64) < seek_frac as f64 {
                        self.drop_frames();
                    }

                    // frame preview
                    if seek_preview {
                        match self.recieve_next_packet_until_frame() {
                            Ok(frame) => apply_processed_frame(frame),
                            _ => (),
                        }
                    }
                }
            };
        }
    }

    /// The stream index.
    fn stream_index(&self) -> usize;
    /// The elapsed time, in milliseconds.
    fn elapsed_ms(&mut self) -> &mut Cache<i64>;
    /// The streamer's decoder.
    fn decoder(&mut self) -> &mut ffmpeg::decoder::Opened;
    /// The streamer's input context.
    fn input_context(&mut self) -> &mut ffmpeg::format::context::Input;
    /// The streamer's state.
    fn player_state(&mut self) -> &mut Cache<PlayerState>;

    /// Output a frame from the decoder.
    fn decode_frame(&mut self) -> Result<Self::Frame>;
    /// Ignore the remainder of this packet.
    fn drop_frames(&mut self) {
        if self.decode_frame().is_err() {
            let _ = self.recieve_next_packet();
        } else {
            self.drop_frames();
        }
    }
    /// Recieve the next packet of the stream.
    fn recieve_next_packet(&mut self) -> Result<()> {
        if let Some((stream, packet)) = self.input_context().packets().next() {
            let time_base = stream.time_base();
            if stream.index() == self.stream_index() {
                self.decoder().send_packet(&packet)?;
                if let Some(dts) = packet.dts() {
                    self.elapsed_ms().set(timestamp_to_millisec(dts, time_base));
                }
            }
        } else {
            self.decoder().send_eof()?;
            self.player_state().set(PlayerState::EndOfFile);
        }
        Ok(())
    }
    /// Reset the stream to its initial state.
    fn reset(&mut self, start_playing: bool) {
        let beginning: i64 = 0;
        let beginning_seek = beginning.rescale((1, 1), rescale::TIME_BASE);
        let _ = self.input_context().seek(beginning_seek, ..beginning_seek);
        self.decoder().flush();

        if start_playing {
            self.player_state().set(PlayerState::Playing);
        }
    }
    /// Keep recieving packets until a frame can be decoded.
    fn recieve_next_packet_until_frame(&mut self) -> Result<Self::ProcessedFrame> {
        match self.recieve_next_frame() {
            Ok(frame_result) => Ok(frame_result),
            Err(e) => {
                if matches!(e.downcast_ref::<ffmpeg::Error>(), Some(ffmpeg::Error::Eof)) {
                    Err(e)
                } else {
                    self.recieve_next_packet()?;
                    self.recieve_next_packet_until_frame()
                }
            }
        }
    }
    /// Process a decoded frame.
    fn process_frame(&mut self, frame: Self::Frame) -> Result<Self::ProcessedFrame>;
    /// Decode and process a frame.
    fn recieve_next_frame(&mut self) -> Result<Self::ProcessedFrame> {
        match self.decode_frame() {
            Ok(decoded_frame) => self.process_frame(decoded_frame),
            Err(e) => {
                return Err(e.into());
            }
        }
    }
}

impl Streamer for VideoStreamer {
    type Frame = Video;
    type ProcessedFrame = ColorImage;
    fn stream_index(&self) -> usize {
        self.video_stream_index
    }
    fn decoder(&mut self) -> &mut ffmpeg::decoder::Opened {
        &mut self.video_decoder.0
    }
    fn input_context(&mut self) -> &mut ffmpeg::format::context::Input {
        &mut self.input_context
    }
    fn elapsed_ms(&mut self) -> &mut Cache<i64> {
        &mut self.video_elapsed_ms
    }
    fn player_state(&mut self) -> &mut Cache<PlayerState> {
        &mut self.player_state
    }
    fn decode_frame(&mut self) -> Result<Self::Frame> {
        let mut decoded_frame = Video::empty();
        self.video_decoder.receive_frame(&mut decoded_frame)?;
        Ok(decoded_frame)
    }
    fn process_frame(&mut self, frame: Self::Frame) -> Result<Self::ProcessedFrame> {
        let mut rgb_frame = Video::empty();
        self.scaler.run(&frame, &mut rgb_frame)?;

        let image = video_frame_to_image(rgb_frame);
        Ok(image)
    }
}

impl Streamer for AudioStreamer {
    type Frame = Audio;
    type ProcessedFrame = ();
    fn stream_index(&self) -> usize {
        self.audio_stream_index
    }
    fn decoder(&mut self) -> &mut ffmpeg::decoder::Opened {
        &mut self.audio_decoder.0
    }
    fn input_context(&mut self) -> &mut ffmpeg::format::context::Input {
        &mut self.input_context
    }
    fn elapsed_ms(&mut self) -> &mut Cache<i64> {
        &mut self.audio_elapsed_ms
    }
    fn player_state(&mut self) -> &mut Cache<PlayerState> {
        &mut self.player_state
    }
    fn decode_frame(&mut self) -> Result<Self::Frame> {
        let mut decoded_frame = Audio::empty();
        self.audio_decoder.receive_frame(&mut decoded_frame)?;
        Ok(decoded_frame)
    }
    fn process_frame(&mut self, frame: Self::Frame) -> Result<Self::ProcessedFrame> {
        let mut resampled_frame = ffmpeg::frame::Audio::empty();
        self.resampler.run(&frame, &mut resampled_frame)?;
        let audio_samples = if resampled_frame.is_packed() {
            packed(&resampled_frame)
        } else {
            resampled_frame.plane(0)
        };
        while self.audio_sample_producer.free_len() < audio_samples.len() {
            // std::thread::sleep(std::time::Duration::from_millis(10));
        }
        self.audio_sample_producer.push_slice(audio_samples);
        Ok(())
    }
}

type FfmpegAudioFormat = ffmpeg::format::Sample;
type FfmpegAudioFormatType = ffmpeg::format::sample::Type;
trait AsFfmpegSample {
    fn to_sample(&self) -> ffmpeg::format::Sample;
}

impl AsFfmpegSample for AudioFormat {
    fn to_sample(&self) -> FfmpegAudioFormat {
        match self {
            AudioFormat::U8 => FfmpegAudioFormat::U8(FfmpegAudioFormatType::Packed),
            AudioFormat::S8 => panic!("unsupported audio format"),
            AudioFormat::U16LSB => panic!("unsupported audio format"),
            AudioFormat::U16MSB => panic!("unsupported audio format"),
            AudioFormat::S16LSB => FfmpegAudioFormat::I16(FfmpegAudioFormatType::Packed),
            AudioFormat::S16MSB => FfmpegAudioFormat::I16(FfmpegAudioFormatType::Packed),
            AudioFormat::S32LSB => FfmpegAudioFormat::I32(FfmpegAudioFormatType::Packed),
            AudioFormat::S32MSB => FfmpegAudioFormat::I32(FfmpegAudioFormatType::Packed),
            AudioFormat::F32LSB => FfmpegAudioFormat::F32(FfmpegAudioFormatType::Packed),
            AudioFormat::F32MSB => FfmpegAudioFormat::F32(FfmpegAudioFormatType::Packed),
        }
    }
}

/// Create a new [`AudioDeviceCallback`]. Required for using audio.
pub fn init_audio_device(audio_sys: &sdl2::AudioSubsystem) -> Result<AudioDevice, String> {
    AudioDeviceCallback::init(audio_sys)
}

/// Pipes audio samples to SDL2.
pub struct AudioDeviceCallback {
    sample_streams: Vec<AudioSampleStream>,
}

struct AudioSampleStream {
    sample_consumer: AudioSampleConsumer,
    audio_volume: Cache<f32>,
}

impl AudioCallback for AudioDeviceCallback {
    type Channel = f32;
    fn callback(&mut self, output: &mut [Self::Channel]) {
        for x in output.iter_mut() {
            *x = self
                .sample_streams
                .iter_mut()
                .map(|s| s.sample_consumer.pop().unwrap_or(0.) * s.audio_volume.get())
                .sum()
        }
    }
}

impl AudioDeviceCallback {
    fn init(audio_sys: &sdl2::AudioSubsystem) -> Result<AudioDevice, String> {
        let audio_spec = AudioSpecDesired {
            freq: Some(44_100),
            channels: Some(2),
            samples: None,
        };
        let device = audio_sys.open_playback(None, &audio_spec, |_spec| AudioDeviceCallback {
            sample_streams: vec![],
        })?;
        Ok(device)
    }
}

#[inline]
// Thanks https://github.com/zmwangx/rust-ffmpeg/issues/72 <3
// Interpret the audio frame's data as packed (alternating channels, 12121212, as opposed to planar 11112222)
fn packed<T: ffmpeg::frame::audio::Sample>(frame: &ffmpeg::frame::Audio) -> &[T] {
    if !frame.is_packed() {
        panic!("data is not packed");
    }

    if !<T as ffmpeg::frame::audio::Sample>::is_valid(frame.format(), frame.channels()) {
        panic!("unsupported type");
    }

    unsafe {
        std::slice::from_raw_parts(
            (*frame.as_ptr()).data[0] as *const T,
            frame.samples() * frame.channels() as usize,
        )
    }
}

fn video_frame_to_image(frame: Video) -> ColorImage {
    let size = [frame.width() as usize, frame.height() as usize];
    let data = frame.data(0);
    let stride = frame.stride(0);
    let pixel_size_bytes = 3;
    let byte_width: usize = pixel_size_bytes * frame.width() as usize;
    let height: usize = frame.height() as usize;
    let mut pixels = vec![];
    for line in 0..height {
        let begin = line * stride;
        let end = begin + byte_width;
        let data_line = &data[begin..end];
        pixels.extend(
            data_line
                .chunks_exact(pixel_size_bytes)
                .map(|p| Color32::from_rgb(p[0], p[1], p[2])),
        )
    }
    ColorImage { size, pixels }
}

// pub fn init() {
//     ffmpeg::init().unwrap();
// }
