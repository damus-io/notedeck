use std::{
    sync::mpsc::TryRecvError,
    time::{Instant, SystemTime},
};

use crate::{GifState, GifStateMap, TextureState, TexturedImage, TexturesCache};
use egui::TextureHandle;

pub fn ensure_latest_texture_from_cache(
    ui: &egui::Ui,
    url: &str,
    gifs: &mut GifStateMap,
    textures: &mut TexturesCache,
) -> Option<TextureHandle> {
    let tstate = textures.cache.get_mut(url)?;

    let TextureState::Loaded(img) = tstate.into() else {
        return None;
    };

    Some(ensure_latest_texture(ui, url, gifs, img))
}

pub fn ensure_latest_texture(
    _ui: &egui::Ui,
    url: &str,
    gifs: &mut GifStateMap,
    img: &mut TexturedImage,
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

            let now = Instant::now();
            let (texture, maybe_new_state, request_next_repaint) = match gifs.get(url) {
                Some(prev_state) => {
                    let should_advance =
                        now - prev_state.last_frame_rendered >= prev_state.last_frame_duration;

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
                                let next_frame_time = SystemTime::now().checked_add(frame.delay);
                                (
                                    &frame.texture,
                                    Some(GifState {
                                        last_frame_rendered: now,
                                        last_frame_duration: frame.delay,
                                        next_frame_time,
                                        last_frame_index: maybe_new_index,
                                    }),
                                    next_frame_time,
                                )
                            }
                            None => {
                                let (tex, state) =
                                    match animation.get_frame(prev_state.last_frame_index) {
                                        Some(frame) => (&frame.texture, None),
                                        None => (&animation.first_frame.texture, None),
                                    };

                                (tex, state, prev_state.next_frame_time)
                            }
                        }
                    } else {
                        let (tex, state) = match animation.get_frame(prev_state.last_frame_index) {
                            Some(frame) => (&frame.texture, None),
                            None => (&animation.first_frame.texture, None),
                        };
                        (tex, state, prev_state.next_frame_time)
                    }
                }
                None => (
                    &animation.first_frame.texture,
                    Some(GifState {
                        last_frame_rendered: now,
                        last_frame_duration: animation.first_frame.delay,
                        next_frame_time: None,
                        last_frame_index: 0,
                    }),
                    None,
                ),
            };

            if let Some(new_state) = maybe_new_state {
                gifs.insert(url.to_owned(), new_state);
            }

            if let Some(_req) = request_next_repaint {
                // TODO(jb55): make a continuous gif rendering setting
                // 24fps for gif is fine
                /*
                tracing::trace!("requesting repaint for {url} after {req:?}");
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(41));
                */
            }

            texture.clone()
        }
    }
}
