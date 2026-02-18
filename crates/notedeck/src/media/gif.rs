use std::{
    collections::{HashMap, VecDeque},
    io::Cursor,
    path::PathBuf,
    time::{Instant, SystemTime},
};

use crate::GifState;
use crate::{
    jobs::{
        CompleteResponse, JobOutput, JobPackage, JobRun, MediaJobKind, MediaJobResult,
        MediaJobSender, NoOutputRun, RunType,
    },
    media::{
        images::{buffer_to_color_image, process_image},
        load_texture_checked,
    },
    Error, ImageFrame, ImageType, MediaCache, TextureFrame, TextureState,
};
use crate::{media::AnimationMode, Animation};
use egui::{ColorImage, TextureHandle};
use image::{codecs::gif::GifDecoder, AnimationDecoder, DynamicImage, Frame};
use std::time::Duration;

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
    let processed_frames = collect_processed_gif_frames(gif_bytes, process_to_egui)?;

    let mut imgs = Vec::with_capacity(processed_frames.len());
    let mut other_frames = Vec::with_capacity(processed_frames.len().saturating_sub(1));

    let mut first_frame = None;
    for (i, processed) in processed_frames.into_iter().enumerate() {
        let ProcessedColorFrame { delay, image: img } = processed;
        imgs.push(ImageFrame {
            delay,
            image: img.clone(),
        });

        let tex_frame = generate_animation_frame(&ctx, &url, i, delay, img);

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

/// Decodes GIF bytes into ordered image frames while preserving timing metadata.
fn decode_gif_frames(gif_bytes: Vec<u8>) -> Result<VecDeque<Frame>, Error> {
    let decoder = {
        let reader = Cursor::new(gif_bytes.as_slice());
        GifDecoder::new(reader)?
    };

    decoder
        .into_frames()
        .collect::<std::result::Result<VecDeque<_>, image::ImageError>>()
        .map_err(|e| crate::Error::Generic(e.to_string()))
}

struct ProcessedColorFrame {
    delay: Duration,
    image: ColorImage,
}

/// Decodes and processes all GIF frames into color images while preserving per-frame delays.
fn collect_processed_gif_frames(
    gif_bytes: Vec<u8>,
    process_to_egui: impl Fn(DynamicImage) -> ColorImage + Send + Copy + 'static,
) -> Result<Vec<ProcessedColorFrame>, Error> {
    let frames = decode_gif_frames(gif_bytes)?;
    let mut processed_frames = Vec::with_capacity(frames.len());
    for frame in frames {
        let delay: Duration = frame.delay().into();
        let image = generate_color_img_frame(frame, process_to_egui);
        processed_frames.push(ProcessedColorFrame { delay, image });
    }

    Ok(processed_frames)
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
