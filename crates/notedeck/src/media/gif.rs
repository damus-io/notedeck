use std::{
    collections::{HashMap, VecDeque},
    io::Cursor,
    path::PathBuf,
};

use crate::GifState;
use crate::{
    jobs::{
        CompleteResponse, JobOutput, JobPackage, JobRun, MediaJobKind, MediaJobResult,
        MediaJobSender, NoOutputRun, RunType,
    },
    media::{
        animated::{process_animation_frame, AnimationBuilder, ProcessedAnimatedFrame},
        images::{buffer_to_color_image, process_image},
    },
    Error, ImageFrame, ImageType, MediaCache, TextureState,
};
use crate::{media::AnimationMode, Animation};
use egui::ColorImage;
use image::{codecs::gif::GifDecoder, AnimationDecoder, DynamicImage, Frame};

pub(crate) type ProcessedGifFrame<'a> = ProcessedAnimatedFrame<'a>;

/// Process a gif state frame, and optionally present a new
/// state and when to repaint it
pub(crate) fn process_gif_frame<'a>(
    animation: &'a Animation,
    frame_state: Option<&GifState>,
    animation_mode: AnimationMode,
) -> ProcessedGifFrame<'a> {
    process_animation_frame(animation, frame_state, animation_mode, true)
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
    let mut animation_builder = AnimationBuilder::new();
    for (i, frame) in frames.into_iter().enumerate() {
        let delay = frame.delay();
        let img = generate_color_img_frame(frame, process_to_egui);
        imgs.push(ImageFrame {
            delay: delay.into(),
            image: img.clone(),
        });

        animation_builder.push_frame(&ctx, &url, i, delay.into(), img);
    }

    Ok(AnimationPackage {
        anim: animation_builder.finish("first frame not found for gif")?,
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
