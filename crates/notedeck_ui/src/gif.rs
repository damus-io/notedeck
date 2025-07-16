use std::{
    sync::mpsc::TryRecvError,
    time::{Instant, SystemTime},
};

use egui::TextureHandle;
use notedeck::{GifState, GifStateMap, TexturedImage};

pub struct LatextTexture<'a> {
    pub texture: &'a TextureHandle,
    pub request_next_repaint: Option<SystemTime>,
}

/// This is necessary because other repaint calls can effectively steal our repaint request.
/// So we must keep on requesting to repaint at our desired time to ensure our repaint goes through.
/// See [`egui::Context::request_repaint_after`]
pub fn handle_repaint<'a>(ui: &egui::Ui, latest: LatextTexture<'a>) -> &'a TextureHandle {
    if let Some(_repaint) = latest.request_next_repaint {
        // 24fps for gif is fine
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(41));
    }
    latest.texture
}

#[must_use = "caller should pass the return value to `gif::handle_repaint`"]
pub fn retrieve_latest_texture<'a>(
    url: &str,
    gifs: &'a mut GifStateMap,
    cached_image: &'a mut TexturedImage,
) -> LatextTexture<'a> {
    match cached_image {
        TexturedImage::Static(texture) => LatextTexture {
            texture,
            request_next_repaint: None,
        },
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

            if let Some(req) = request_next_repaint {
                tracing::trace!("requesting repaint for {url} after {req:?}");
            }

            LatextTexture {
                texture,
                request_next_repaint,
            }
        }
    }
}
