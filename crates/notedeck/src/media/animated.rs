use std::time::{Duration, Instant, SystemTime};

use egui::{ColorImage, TextureHandle};

use crate::{media::load_texture_checked, media::AnimationMode, Animation, Error, TextureFrame};

/// Per-image playback state for animated media.
pub struct AnimatedFrameState {
    pub last_frame_rendered: Instant,
    pub last_frame_duration: Duration,
    pub next_frame_time: Option<SystemTime>,
    pub last_frame_index: usize,
}

/// Result of advancing (or holding) an animated frame sequence.
pub struct ProcessedAnimatedFrame<'a> {
    pub texture: &'a TextureHandle,
    pub maybe_new_state: Option<AnimatedFrameState>,
    pub repaint_at: Option<SystemTime>,
}

/// Advances animation playback based on mode and prior state.
pub fn process_animation_frame<'a>(
    animation: &'a Animation,
    frame_state: Option<&AnimatedFrameState>,
    animation_mode: AnimationMode,
    schedule_first_frame_repaint: bool,
) -> ProcessedAnimatedFrame<'a> {
    let now = Instant::now();

    let Some(prev_state) = frame_state else {
        let next_frame_time = if schedule_first_frame_repaint {
            compute_next_frame_time(animation_mode, animation.first_frame.delay)
        } else {
            None
        };
        return ProcessedAnimatedFrame {
            texture: &animation.first_frame.texture,
            maybe_new_state: Some(AnimatedFrameState {
                last_frame_rendered: now,
                last_frame_duration: animation.first_frame.delay,
                next_frame_time,
                last_frame_index: 0,
            }),
            repaint_at: next_frame_time,
        };
    };

    let should_advance = animation_mode.can_animate()
        && (now - prev_state.last_frame_rendered >= prev_state.last_frame_duration);

    if !should_advance {
        return ProcessedAnimatedFrame {
            texture: animation
                .get_frame(prev_state.last_frame_index)
                .map_or(&animation.first_frame.texture, |frame| &frame.texture),
            maybe_new_state: None,
            repaint_at: prev_state.next_frame_time,
        };
    }

    let maybe_new_index = if prev_state.last_frame_index < animation.num_frames() - 1 {
        prev_state.last_frame_index + 1
    } else {
        0
    };

    let Some(frame) = animation.get_frame(maybe_new_index) else {
        return ProcessedAnimatedFrame {
            texture: animation
                .get_frame(prev_state.last_frame_index)
                .map_or(&animation.first_frame.texture, |current| &current.texture),
            maybe_new_state: None,
            repaint_at: prev_state.next_frame_time,
        };
    };

    let next_frame_time = compute_next_frame_time(animation_mode, frame.delay);
    ProcessedAnimatedFrame {
        texture: &frame.texture,
        maybe_new_state: Some(AnimatedFrameState {
            last_frame_rendered: now,
            last_frame_duration: frame.delay,
            next_frame_time,
            last_frame_index: maybe_new_index,
        }),
        repaint_at: next_frame_time,
    }
}

/// Calculates when to request a repaint for the next animation frame.
pub fn compute_next_frame_time(
    animation_mode: AnimationMode,
    frame_delay: Duration,
) -> Option<SystemTime> {
    match animation_mode {
        AnimationMode::Continuous { fps } => match fps {
            Some(fps) => {
                let max_delay_ms = Duration::from_millis((1000.0 / fps) as u64);
                SystemTime::now().checked_add(frame_delay.max(max_delay_ms))
            }
            None => SystemTime::now().checked_add(frame_delay),
        },
        AnimationMode::NoAnimation | AnimationMode::Reactive => None,
    }
}

/// Helper for building texture-backed `Animation` values frame by frame.
pub struct AnimationBuilder {
    first_frame: Option<TextureFrame>,
    other_frames: Vec<TextureFrame>,
}

impl AnimationBuilder {
    pub fn new() -> Self {
        Self {
            first_frame: None,
            other_frames: Vec::new(),
        }
    }

    pub fn push_frame(
        &mut self,
        ctx: &egui::Context,
        url: &str,
        index: usize,
        delay: Duration,
        color_img: ColorImage,
    ) {
        let tex_frame = TextureFrame {
            delay,
            texture: load_texture_checked(
                ctx,
                format!("{url}{index}"),
                color_img,
                Default::default(),
            ),
        };

        if self.first_frame.is_none() {
            self.first_frame = Some(tex_frame);
        } else {
            self.other_frames.push(tex_frame);
        }
    }

    pub fn finish(self, missing_first_frame_error: &'static str) -> Result<Animation, Error> {
        let Some(first_frame) = self.first_frame else {
            return Err(crate::Error::Generic(missing_first_frame_error.to_owned()));
        };

        Ok(Animation {
            first_frame,
            other_frames: self.other_frames,
        })
    }
}

impl Default for AnimationBuilder {
    fn default() -> Self {
        Self::new()
    }
}
