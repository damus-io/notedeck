/// Returns true on the first frame a bool condition becomes true.
/// Uses egui temp storage to track previous state per ID.
pub fn state_entered(ctx: &egui::Context, id: egui::Id, active: bool) -> bool {
    let was_active: bool = ctx.data(|d| d.get_temp(id).unwrap_or(false));
    if active != was_active {
        ctx.data_mut(|d| d.insert_temp(id, active));
    }
    active && !was_active
}

/// Convenience: returns true on the first frame a widget becomes hovered.
pub fn hover_entered(ui: &egui::Ui, id: egui::Id, hovered: bool) -> bool {
    state_entered(ui.ctx(), id, hovered)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SoundEffect {
    Hover,
    Click,
    Like,
    Repost,
    Zap,
    ZapConfirm,
    Send,
    Notification,
    Opened,
    Closed,
    Startup,
}

#[cfg(feature = "sound")]
mod inner {
    use super::SoundEffect;
    use std::collections::HashMap;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
    use std::time::Instant;
    use tracing::{info, warn};

    pub struct SoundManager {
        handle: Option<rodio::OutputStreamHandle>,
        _stream: Option<rodio::OutputStream>,
        sounds: HashMap<SoundEffect, &'static [u8]>,
        enabled: AtomicBool,
        volume: AtomicU32,
        /// Per-effect deadline (millis since creation). 0 = nothing pending.
        pending: HashMap<SoundEffect, AtomicU64>,
        created_at: Instant,
    }

    impl SoundManager {
        pub fn new(enabled: bool, volume: f32) -> Self {
            let (stream, handle) = match rodio::OutputStream::try_default() {
                Ok((stream, handle)) => {
                    info!("Audio output initialized");
                    (Some(stream), Some(handle))
                }
                Err(e) => {
                    warn!("No audio device available: {e}");
                    (None, None)
                }
            };

            let mut sounds = HashMap::new();
            sounds.insert(
                SoundEffect::Hover,
                include_bytes!("../../../assets/sounds/hover2.mp3").as_slice(),
            );
            sounds.insert(
                SoundEffect::Click,
                include_bytes!("../../../assets/sounds/click3.mp3").as_slice(),
            );
            sounds.insert(
                SoundEffect::Like,
                include_bytes!("../../../assets/sounds/click2.mp3").as_slice(),
            );
            sounds.insert(
                SoundEffect::Repost,
                include_bytes!("../../../assets/sounds/click3.mp3").as_slice(),
            );
            sounds.insert(
                SoundEffect::Zap,
                include_bytes!("../../../assets/sounds/zap.mp3").as_slice(),
            );
            sounds.insert(
                SoundEffect::ZapConfirm,
                include_bytes!("../../../assets/sounds/pay-success.mp3").as_slice(),
            );
            sounds.insert(
                SoundEffect::Send,
                include_bytes!("../../../assets/sounds/woosh.mp3").as_slice(),
            );
            sounds.insert(
                SoundEffect::Notification,
                include_bytes!("../../../assets/sounds/new-message.mp3").as_slice(),
            );
            sounds.insert(
                SoundEffect::Opened,
                include_bytes!("../../../assets/sounds/open.mp3").as_slice(),
            );
            sounds.insert(
                SoundEffect::Closed,
                include_bytes!("../../../assets/sounds/close.mp3").as_slice(),
            );
            sounds.insert(
                SoundEffect::Startup,
                include_bytes!("../../../assets/sounds/startup.mp3").as_slice(),
            );

            let pending: HashMap<SoundEffect, AtomicU64> =
                sounds.keys().map(|k| (*k, AtomicU64::new(0))).collect();

            Self {
                handle,
                _stream: stream,
                sounds,
                enabled: AtomicBool::new(enabled),
                volume: AtomicU32::new(volume.to_bits()),
                pending,
                created_at: Instant::now(),
            }
        }

        pub fn play(&self, effect: SoundEffect) {
            if !self.enabled.load(Ordering::Relaxed) {
                return;
            }

            let Some(handle) = &self.handle else {
                return;
            };

            let Some(bytes) = self.sounds.get(&effect) else {
                return;
            };

            let cursor = Cursor::new(*bytes);
            let Ok(source) = rodio::Decoder::new(cursor) else {
                warn!("Failed to decode sound: {:?}", effect);
                return;
            };

            let volume = f32::from_bits(self.volume.load(Ordering::Relaxed));
            use rodio::Source;
            if let Err(e) = handle.play_raw(source.amplify(volume).convert_samples()) {
                warn!("Failed to play sound {:?}: {e}", effect);
            }
        }

        /// Schedule a debounced play. Each call resets the timer. The sound
        /// only fires once `debounce_ms` elapses without another call.
        /// Call `update()` each frame to flush pending sounds.
        pub fn play_debounced(&self, effect: SoundEffect, debounce_ms: u64) {
            let deadline = self.created_at.elapsed().as_millis() as u64 + debounce_ms;
            if let Some(slot) = self.pending.get(&effect) {
                slot.store(deadline, Ordering::Relaxed);
            }
        }

        /// Call once per frame to fire any debounced sounds whose deadline
        /// has passed.
        pub fn update(&self) {
            let now_ms = self.created_at.elapsed().as_millis() as u64;
            for (effect, slot) in &self.pending {
                let deadline = slot.load(Ordering::Relaxed);
                if deadline != 0 && now_ms >= deadline {
                    slot.store(0, Ordering::Relaxed);
                    self.play(*effect);
                }
            }
        }

        pub fn set_volume(&self, volume: f32) {
            self.volume.store(volume.to_bits(), Ordering::Relaxed);
        }

        pub fn set_enabled(&self, enabled: bool) {
            self.enabled.store(enabled, Ordering::Relaxed);
        }
    }
}

#[cfg(not(feature = "sound"))]
mod inner {
    use super::SoundEffect;

    pub struct SoundManager;

    impl SoundManager {
        pub fn new(_enabled: bool, _volume: f32) -> Self {
            Self
        }

        pub fn play(&self, _effect: SoundEffect) {}

        pub fn play_debounced(&self, _effect: SoundEffect, _debounce_ms: u64) {}

        pub fn update(&self) {}

        pub fn set_volume(&self, _volume: f32) {}

        pub fn set_enabled(&self, _enabled: bool) {}
    }
}

pub use inner::SoundManager;
