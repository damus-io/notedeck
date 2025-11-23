use std::{
    collections::{HashMap, VecDeque},
    io::Cursor,
    path::PathBuf,
    sync::mpsc::TryRecvError,
    time::{Instant, SystemTime},
};

use crate::{
    jobs::{
        CompleteResponse, JobOutput, JobPackage, JobRun, MediaJobKind, MediaJobResult,
        MediaJobSender, NoOutputRun, RunType,
    },
    media::{
        images::{buffer_to_color_image, process_image},
        load_texture_checked,
    },
    AnimationOld, Error, ImageFrame, ImageType, MediaCache, TextureFrame, TextureState,
};
use crate::{media::AnimationMode, Animation};
use crate::{GifState, GifStateMap, TextureStateOld, TexturedImage, TexturesCache};
use egui::{ColorImage, TextureHandle};
use image::{codecs::gif::GifDecoder, AnimationDecoder, DynamicImage, Frame};
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

pub struct AnimatedImgTexCache {
    pub(crate) cache: HashMap<String, TextureState<Animation>>,
    animated_img_cache_path: PathBuf,
}

impl AnimatedImgTexCache {
    pub fn new(animated_img_cache_path: PathBuf) -> Self {
        Self {
            cache: Default::default(),
            animated_img_cache_path,
        }
    }

    pub fn contains(&self, url: &str) -> bool {
        self.cache.contains_key(url)
    }

    pub fn get(&self, url: &str) -> Option<&TextureState<Animation>> {
        self.cache.get(url)
    }

    pub fn request(
        &self,
        jobs: &MediaJobSender,
        ctx: &egui::Context,
        url: &str,
        imgtype: ImageType,
    ) {
        let _ = self.get_or_request(jobs, ctx, url, imgtype);
    }

    pub fn get_or_request(
        &self,
        jobs: &MediaJobSender,
        ctx: &egui::Context,
        url: &str,
        imgtype: ImageType,
    ) -> &TextureState<Animation> {
        if let Some(res) = self.cache.get(url) {
            return res;
        };

        let key = MediaCache::key(url);
        let path = self.animated_img_cache_path.join(key);
        let ctx = ctx.clone();
        let url = url.to_owned();
        if path.exists() {
            if let Err(e) = jobs.send(JobPackage::new(
                url.to_owned(),
                MediaJobKind::AnimatedImg,
                RunType::Output(JobRun::Sync(Box::new(move || {
                    from_disk_job_run(ctx, url, path)
                }))),
            )) {
                tracing::error!("{e}");
            }
        } else {
            let anim_path = self.animated_img_cache_path.clone();
            if let Err(e) = jobs.send(JobPackage::new(
                url.to_owned(),
                MediaJobKind::AnimatedImg,
                RunType::Output(JobRun::Async(Box::pin(from_net_run(
                    ctx, url, anim_path, imgtype,
                )))),
            )) {
                tracing::error!("{e}");
            }
        }

        &TextureState::Pending
    }
}

fn from_disk_job_run(ctx: egui::Context, url: String, path: PathBuf) -> JobOutput<MediaJobResult> {
    tracing::trace!("Starting animated from disk job for {url}");
    let gif_bytes = match std::fs::read(path.clone()) {
        Ok(b) => b,
        Err(e) => {
            return JobOutput::Complete(CompleteResponse::new(MediaJobResult::Animation(Err(
                Error::Io(e),
            ))))
        }
    };
    JobOutput::Complete(CompleteResponse::new(MediaJobResult::Animation(
        generate_anim_pkg(ctx.clone(), url.to_owned(), gif_bytes, |img| {
            buffer_to_color_image(img.as_flat_samples_u8(), img.width(), img.height())
        })
        .map(|f| f.anim),
    )))
}

async fn from_net_run(
    ctx: egui::Context,
    url: String,
    path: PathBuf,
    imgtype: ImageType,
) -> JobOutput<MediaJobResult> {
    let res = match crate::media::network::http_req(&url).await {
        Ok(r) => r,
        Err(e) => {
            return JobOutput::complete(MediaJobResult::Animation(Err(crate::Error::Generic(
                format!("Http error: {e}"),
            ))));
        }
    };

    JobOutput::Next(JobRun::Sync(Box::new(move || {
        tracing::trace!("Starting animated img from net job for {url}");
        let animation =
            match generate_anim_pkg(ctx.clone(), url.to_owned(), res.bytes, move |img| {
                process_image(imgtype, img)
            }) {
                Ok(a) => a,
                Err(e) => {
                    return JobOutput::Complete(CompleteResponse::new(MediaJobResult::Animation(
                        Err(e),
                    )));
                }
            };
        JobOutput::Complete(
            CompleteResponse::new(MediaJobResult::Animation(Ok(animation.anim))).run_no_output(
                NoOutputRun::Sync(Box::new(move || {
                    tracing::trace!("writing animated texture to file for {url}");
                    if let Err(e) = MediaCache::write_gif(&path, &url, animation.img_frames) {
                        tracing::error!("Could not write gif to disk: {e}");
                    }
                })),
            ),
        )
    })))
}

fn generate_anim_pkg(
    ctx: egui::Context,
    url: String,
    gif_bytes: Vec<u8>,
    process_to_egui: impl Fn(DynamicImage) -> ColorImage + Send + Copy + 'static,
) -> Result<AnimationPackage, Error> {
    let decoder = {
        let reader = Cursor::new(gif_bytes.as_slice());
        GifDecoder::new(reader)?
    };

    let frames: VecDeque<Frame> = decoder
        .into_frames()
        .collect::<std::result::Result<VecDeque<_>, image::ImageError>>()
        .map_err(|e| crate::Error::Generic(e.to_string()))?;

    let mut imgs = Vec::new();
    let mut other_frames = Vec::new();

    let mut first_frame = None;
    for (i, frame) in frames.into_iter().enumerate() {
        let delay = frame.delay();
        let img = generate_color_img_frame(frame, process_to_egui);
        imgs.push(ImageFrame {
            delay: delay.into(),
            image: img.clone(),
        });

        let tex_frame = generate_animation_frame(&ctx, &url, i, delay.into(), img);

        if first_frame.is_none() {
            first_frame = Some(tex_frame);
        } else {
            other_frames.push(tex_frame);
        }
    }

    let Some(first_frame) = first_frame else {
        return Err(crate::Error::Generic(
            "first frame not found for gif".to_owned(),
        ));
    };

    Ok(AnimationPackage {
        anim: Animation {
            first_frame,
            other_frames,
        },
        img_frames: imgs,
    })
}

struct AnimationPackage {
    anim: Animation,
    img_frames: Vec<ImageFrame>,
}

fn generate_color_img_frame(
    frame: image::Frame,
    process_to_egui: impl Fn(DynamicImage) -> ColorImage + Send + Copy + 'static,
) -> ColorImage {
    let img = DynamicImage::ImageRgba8(frame.into_buffer());
    process_to_egui(img)
}

fn generate_animation_frame(
    ctx: &egui::Context,
    url: &str,
    index: usize,
    delay: Duration,
    color_img: ColorImage,
) -> TextureFrame {
    TextureFrame {
        delay,
        texture: load_texture_checked(ctx, format!("{url}{index}"), color_img, Default::default()),
    }
}
