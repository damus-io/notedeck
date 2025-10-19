//! Stub implementation used on platforms without ffmpeg/SDL support.

use anyhow::{anyhow, Result};
use egui::{vec2, Color32, FontId, Response, Sense, Ui};

/// Placeholder audio device type on unsupported platforms.
pub type AudioDevice = ();

/// Simplified cache wrapper mirroring the desktop API surface.
#[derive(Clone)]
pub struct Cache<T: Copy> {
    value: T,
}

impl<T: Copy> Cache<T> {
    /// Create a new cache wrapper.
    pub fn new(value: T) -> Self {
        Self { value }
    }

    /// Overwrite the cached value.
    pub fn set(&mut self, value: T) {
        self.value = value;
    }

    /// Read the cached value.
    pub fn get(&mut self) -> T {
        self.value
    }

    /// Read the cached value.
    pub fn get_true(&mut self) -> T {
        self.value
    }
}

/// Playback state placeholder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlayerState {
    /// No playback available.
    Stopped,
    /// End-of-file marker.
    EndOfFile,
    /// Seek request.
    Seeking(f32),
    /// Paused playback.
    Paused,
    /// Actively playing.
    Playing,
}

/// Minimal stand-in for the desktop player.
#[allow(dead_code)]
pub struct Player {
    /// Fake framerate exposed for callers.
    pub framerate: f64,
    /// Cached state wrapper.
    pub player_state: Cache<PlayerState>,
    /// Reported video height.
    pub height: u32,
    /// Reported video width.
    pub width: u32,
    /// Looping flag.
    pub looping: bool,
    /// Volume cache.
    pub audio_volume: Cache<f32>,
    /// Max volume placeholder.
    pub max_audio_volume: f32,
    /// Fake audio stream indicator.
    pub audio_streamer: Option<()>,
}

impl Player {
    /// Attempt to create a player – always errors on unsupported platforms.
    pub fn new(_ctx: &egui::Context, _input_path: &String) -> Result<Self> {
        Err(anyhow!(
            "embedded video playback is not supported on this platform"
        ))
    }

    /// Attempt to create a player from bytes – always errors.
    #[cfg(feature = "from_bytes")]
    pub fn new_from_bytes(_ctx: &egui::Context, _input_bytes: &[u8]) -> Result<Self> {
        Err(anyhow!(
            "embedded video playback is not supported on this platform"
        ))
    }

    /// No-op audio wiring; returns the player unchanged.
    pub fn with_audio(self, _audio_device: &mut AudioDevice) -> Result<Self> {
        Ok(self)
    }

    /// Render a placeholder widget to inform the user.
    pub fn ui(&mut self, ui: &mut Ui, size: [f32; 2]) -> Response {
        let desired_size = vec2(size[0], size[1]);
        let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
        ui.painter()
            .rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Video playback unavailable",
            FontId::proportional(16.0),
            Color32::from_gray(160),
        );
        response
    }

    /// Render at an explicit rectangle; delegates to [`Self::ui`].
    pub fn ui_at(&mut self, ui: &mut Ui, rect: egui::Rect) -> Response {
        ui.allocate_rect(rect, Sense::click());
        self.ui(ui, [rect.width(), rect.height()])
    }

    /// Return a placeholder duration string.
    pub fn duration_text(&mut self) -> String {
        "0:00 / 0:00".to_string()
    }

    /// Transition into playing state.
    pub fn start(&mut self) {
        self.player_state.set(PlayerState::Playing);
    }

    /// Transition into paused state.
    pub fn pause(&mut self) {
        self.player_state.set(PlayerState::Paused);
    }

    /// Transition back to playing state.
    pub fn unpause(&mut self) {
        self.player_state.set(PlayerState::Playing);
    }

    /// Transition into stopped state.
    pub fn stop(&mut self) {
        self.player_state.set(PlayerState::Stopped);
    }
}

/// Initialize audio – always fails on unsupported platforms.
pub fn init_audio_device<T>(_audio_sys: &T) -> Result<AudioDevice, String> {
    Err("audio playback is not supported on this platform".to_string())
}

/// Initialize ffmpeg – noop on unsupported platforms.
pub fn ensure_initialized() {}
