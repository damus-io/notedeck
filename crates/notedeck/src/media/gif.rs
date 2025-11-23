use std::{
    sync::mpsc::TryRecvError,
    time::{Instant, SystemTime},
};

use crate::AnimationOld;
use crate::{media::AnimationMode, Animation};
use crate::{GifState, GifStateMap, TextureStateOld, TexturedImage, TexturesCache};
use egui::TextureHandle;
use std::time::Duration;

pub fn ensure_latest_texture_from_cache(
    ui: &egui::Ui,
    url: &str,
    gifs: &mut GifStateMap,
    textures: &mut TexturesCache,
    animation_mode: AnimationMode,
) -> Option<TextureHandle> {
    let tstate = textures.cache.get_mut(url)?;

    let TextureStateOld::Loaded(img) = tstate.into() else {
        return None;
    };

    Some(ensure_latest_texture(ui, url, gifs, img, animation_mode))
}

struct ProcessedGifFrameOld {
    texture: TextureHandle,
    maybe_new_state: Option<GifState>,
    repaint_at: Option<SystemTime>,
}

/// Process a gif state frame, and optionally present a new
/// state and when to repaint it
fn process_gif_frame_old(
    animation: &AnimationOld,
    frame_state: Option<&GifState>,
    animation_mode: AnimationMode,
) -> ProcessedGifFrameOld {
    let now = Instant::now();

    match frame_state {
        Some(prev_state) => {
            let should_advance = animation_mode.can_animate()
                && (now - prev_state.last_frame_rendered >= prev_state.last_frame_duration);

            if should_advance {
                let maybe_new_index = if animation.receiver.is_some()
                    || prev_state.last_frame_index < animation.num_frames() - 1
                {
                    prev_state.last_frame_index + 1
                } else {
                    0
                };

                match animation.get_frame(maybe_new_index) {
                    Some(frame) => {
                        let next_frame_time = match animation_mode {
                            AnimationMode::Continuous { fps } => match fps {
                                Some(fps) => {
                                    let max_delay_ms = Duration::from_millis((1000.0 / fps) as u64);
                                    SystemTime::now().checked_add(frame.delay.max(max_delay_ms))
                                }
                                None => SystemTime::now().checked_add(frame.delay),
                            },

                            AnimationMode::NoAnimation | AnimationMode::Reactive => None,
                        };

                        ProcessedGifFrameOld {
                            texture: frame.texture.clone(),
                            maybe_new_state: Some(GifState {
                                last_frame_rendered: now,
                                last_frame_duration: frame.delay,
                                next_frame_time,
                                last_frame_index: maybe_new_index,
                            }),
                            repaint_at: next_frame_time,
                        }
                    }
                    None => {
                        let (texture, maybe_new_state) =
                            match animation.get_frame(prev_state.last_frame_index) {
                                Some(frame) => (frame.texture.clone(), None),
                                None => (animation.first_frame.texture.clone(), None),
                            };

                        ProcessedGifFrameOld {
                            texture,
                            maybe_new_state,
                            repaint_at: prev_state.next_frame_time,
                        }
                    }
                }
            } else {
                let (texture, maybe_new_state) =
                    match animation.get_frame(prev_state.last_frame_index) {
                        Some(frame) => (frame.texture.clone(), None),
                        None => (animation.first_frame.texture.clone(), None),
                    };

                ProcessedGifFrameOld {
                    texture,
                    maybe_new_state,
                    repaint_at: prev_state.next_frame_time,
                }
            }
        }
        None => ProcessedGifFrameOld {
            texture: animation.first_frame.texture.clone(),
            maybe_new_state: Some(GifState {
                last_frame_rendered: now,
                last_frame_duration: animation.first_frame.delay,
                next_frame_time: None,
                last_frame_index: 0,
            }),
            repaint_at: None,
        },
    }
}

pub(crate) struct ProcessedGifFrame<'a> {
    pub texture: &'a TextureHandle,
    pub maybe_new_state: Option<GifState>,
    pub repaint_at: Option<SystemTime>,
}

/// Process a gif state frame, and optionally present a new
/// state and when to repaint it
pub(crate) fn process_gif_frame<'a>(
    animation: &'a Animation,
    frame_state: Option<&GifState>,
    animation_mode: AnimationMode,
) -> ProcessedGifFrame<'a> {
    let now = Instant::now();

    let Some(prev_state) = frame_state else {
        return ProcessedGifFrame {
            texture: &animation.first_frame.texture,
            maybe_new_state: Some(GifState {
                last_frame_rendered: now,
                last_frame_duration: animation.first_frame.delay,
                next_frame_time: None,
                last_frame_index: 0,
            }),
            repaint_at: None,
        };
    };

    let should_advance = animation_mode.can_animate()
        && (now - prev_state.last_frame_rendered >= prev_state.last_frame_duration);

    if !should_advance {
        let (texture, maybe_new_state) = match animation.get_frame(prev_state.last_frame_index) {
            Some(frame) => (&frame.texture, None),
            None => (&animation.first_frame.texture, None),
        };

        return ProcessedGifFrame {
            texture,
            maybe_new_state,
            repaint_at: prev_state.next_frame_time,
        };
    }

    let maybe_new_index = if prev_state.last_frame_index < animation.num_frames() - 1 {
        prev_state.last_frame_index + 1
    } else {
        0
    };

    let Some(frame) = animation.get_frame(maybe_new_index) else {
        let (texture, maybe_new_state) = match animation.get_frame(prev_state.last_frame_index) {
            Some(frame) => (&frame.texture, None),
            None => (&animation.first_frame.texture, None),
        };

        return ProcessedGifFrame {
            texture,
            maybe_new_state,
            repaint_at: prev_state.next_frame_time,
        };
    };

    let next_frame_time = match animation_mode {
        AnimationMode::Continuous { fps } => match fps {
            Some(fps) => {
                let max_delay_ms = Duration::from_millis((1000.0 / fps) as u64);
                SystemTime::now().checked_add(frame.delay.max(max_delay_ms))
            }
            None => SystemTime::now().checked_add(frame.delay),
        },

        AnimationMode::NoAnimation | AnimationMode::Reactive => None,
    };

    ProcessedGifFrame {
        texture: &frame.texture,
        maybe_new_state: Some(GifState {
            last_frame_rendered: now,
            last_frame_duration: frame.delay,
            next_frame_time,
            last_frame_index: maybe_new_index,
        }),
        repaint_at: next_frame_time,
    }
}

pub fn ensure_latest_texture(
    ui: &egui::Ui,
    url: &str,
    gifs: &mut GifStateMap,
    img: &mut TexturedImage,
    animation_mode: AnimationMode,
) -> TextureHandle {
    match img {
        TexturedImage::Static(handle) => handle.clone(),
        TexturedImage::Animated(animation) => {
            if let Some(receiver) = &animation.receiver {
                loop {
                    match receiver.try_recv() {
                        Ok(frame) => animation.other_frames.push(frame),
                        Err(TryRecvError::Empty) => {
                            break;
                        }
                        Err(TryRecvError::Disconnected) => {
                            animation.receiver = None;
                            break;
                        }
                    }
                }
            }

            let next_state = process_gif_frame_old(animation, gifs.get(url), animation_mode);

            if let Some(new_state) = next_state.maybe_new_state {
                gifs.insert(url.to_owned(), new_state);
            }

            if let Some(repaint) = next_state.repaint_at {
                tracing::trace!("requesting repaint for {url} after {repaint:?}");
                if let Ok(dur) = repaint.duration_since(SystemTime::now()) {
                    ui.ctx().request_repaint_after(dur);
                }
            }

            next_state.texture
        }
    }
}
