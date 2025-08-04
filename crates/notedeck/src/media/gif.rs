use std::{
    sync::mpsc::TryRecvError,
    time::{Instant, SystemTime},
};

use crate::media::AnimationMode;
use crate::Animation;
use crate::{GifState, GifStateMap, TextureState, TexturedImage, TexturesCache};
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

    let TextureState::Loaded(img) = tstate.into() else {
        return None;
    };

    Some(ensure_latest_texture(ui, url, gifs, img, animation_mode))
}

struct ProcessedGifFrame {
    texture: TextureHandle,
    maybe_new_state: Option<GifState>,
    repaint_at: Option<SystemTime>,
}

/// Process a gif state frame, and optionally present a new
/// state and when to repaint it
fn process_gif_frame(
    animation: &Animation,
    frame_state: Option<&GifState>,
    animation_mode: AnimationMode,
) -> ProcessedGifFrame {
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

                        ProcessedGifFrame {
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

                        ProcessedGifFrame {
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

                ProcessedGifFrame {
                    texture,
                    maybe_new_state,
                    repaint_at: prev_state.next_frame_time,
                }
            }
        }
        None => ProcessedGifFrame {
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

            let next_state = process_gif_frame(animation, gifs.get(url), animation_mode);

            if let Some(new_state) = next_state.maybe_new_state {
                gifs.insert(url.to_owned(), new_state);
            }

            if let Some(req) = next_state.repaint_at {
                // TODO(jb55): make a continuous gif rendering setting
                // 24fps for gif is fine
                tracing::trace!("requesting repaint for {url} after {req:?}");
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(41));
            }

            next_state.texture
        }
    }
}
