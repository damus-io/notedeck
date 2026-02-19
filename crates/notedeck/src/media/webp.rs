use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Instant, SystemTime},
};

use crate::imgcache::WebpState;
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
use image::{DynamicImage, RgbImage, Rgba, RgbaImage};
use std::time::Duration;
use webp::{AnimDecoder, BitstreamFeatures};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebpType {
    Static,
    Animated,
}

/// Detects whether a WebP image is static or animated
///
/// This function analyzes the raw WebP data to determine if it contains
/// a single frame (static) or multiple frames (animated).
///
/// # Arguments
///
/// * `webp_bytes` - Raw WebP file data
///
/// # Returns
///
/// Returns `WebpType::Static` for single-frame images, `WebpType::Animated`
/// for multi-frame images. On error, defaults to `WebpType::Static`.
pub fn detect_webp_type(webp_bytes: &[u8]) -> WebpType {
    if let Some(bit_stream) = BitstreamFeatures::new(webp_bytes) {
        return match bit_stream.has_animation() {
            true => WebpType::Animated,
            false => WebpType::Static,
        };
    }

    tracing::warn!("Failed to detect webp type, defaulting to Static");
    WebpType::Static
}

/// Decodes a static WebP image and returns a texture handle
///
/// This function is used when a WebP is detected as static.
/// It decodes the image data and creates an egui texture.
///
/// # Arguments
///
/// * `ctx` - The egui context for texture creation
/// * `url` - The URL of the image (used as texture name)
/// * `webp_bytes` - Raw WebP file data
///
/// # Returns
///
/// Returns a `TextureHandle` on success or an `Error` on failure.
fn decode_static_webp(
    ctx: &egui::Context,
    url: &str,
    webp_bytes: &[u8],
) -> Result<TextureHandle, Error> {
    let decoder = webp::Decoder::new(webp_bytes);

    let image = decoder
        .decode()
        .ok_or_else(|| Error::Generic("Failed to decode static webp".to_string()))?;

    let size = [image.width() as usize, image.height() as usize];
    let color_image = match image.layout() {
        webp::PixelLayout::Rgb => ColorImage::from_rgb(size, image.as_ref()),
        webp::PixelLayout::Rgba => ColorImage::from_rgba_unmultiplied(size, image.as_ref()),
    };

    Ok(load_texture_checked(
        ctx,
        url,
        color_image,
        Default::default(),
    ))
}

/// Decodes a static WebP image with processing and returns a ColorImage
///
/// Similar to `decode_static_webp` but applies image processing (e.g., resizing)
/// before returning the ColorImage. This is used for images fetched from the network.
///
/// # Arguments
///
/// * `ctx` - The egui context (unused but kept for consistency)
/// * `url` - The URL of the image
/// * `webp_bytes` - Raw WebP file data
/// * `imgtype` - The image type for processing (profile, content, etc.)
///
/// # Returns
///
/// Returns a `ColorImage` on success or an `Error` on failure.
fn decode_static_webp_processed(
    _ctx: &egui::Context,
    _url: &str,
    webp_bytes: &[u8],
    imgtype: ImageType,
) -> Result<ColorImage, Error> {
    let decoder = webp::Decoder::new(webp_bytes);

    let image = decoder
        .decode()
        .ok_or_else(|| Error::Generic("Failed to decode static webp".to_string()))?;

    Ok(process_image(imgtype, image.to_image()))
}

pub(crate) struct ProcessedWebpFrame<'a> {
    pub texture: &'a TextureHandle,
    pub maybe_new_state: Option<WebpState>,
    pub repaint_at: Option<SystemTime>,
}

/// Process a webp state frame, and optionally present a new
/// state and when to repaint it
pub(crate) fn process_webp_frame<'a>(
    animation: &'a Animation,
    frame_state: Option<&WebpState>,
    animation_mode: AnimationMode,
) -> ProcessedWebpFrame<'a> {
    let now = Instant::now();

    let Some(prev_state) = frame_state else {
        return ProcessedWebpFrame {
            texture: &animation.first_frame.texture,
            maybe_new_state: Some(WebpState {
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

        return ProcessedWebpFrame {
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

        return ProcessedWebpFrame {
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

    ProcessedWebpFrame {
        texture: &frame.texture,
        maybe_new_state: Some(WebpState {
            last_frame_rendered: now,
            last_frame_duration: frame.delay,
            next_frame_time,
            last_frame_index: maybe_new_index,
        }),
        repaint_at: next_frame_time,
    }
}

pub enum WebpCacheEntry {
    Animated(Animation),
    Static(TextureHandle),
}

/// Cache for WebP textures (both static and animated)
pub struct WebpTexCache {
    pub(crate) cache: HashMap<String, TextureState<WebpCacheEntry>>,
    path: PathBuf,
}

impl WebpTexCache {
    /// Creates a new WebP texture cache
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the directory where cached WebP files are stored
    pub fn new(path: PathBuf) -> Self {
        Self {
            cache: Default::default(),
            path,
        }
    }

    pub fn contains(&self, url: &str) -> bool {
        self.cache.contains_key(url)
    }

    pub fn get(&self, url: &str) -> Option<&TextureState<WebpCacheEntry>> {
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
    ) -> &TextureState<WebpCacheEntry> {
        if let Some(res) = self.cache.get(url) {
            return res;
        };

        let key = MediaCache::key(url);
        let path = self.path.join(key);
        let ctx = ctx.clone();
        let url = url.to_owned();
        if path.exists() {
            if let Err(e) = jobs.send(JobPackage::new(
                url.to_owned(),
                MediaJobKind::WebpImg,
                RunType::Output(JobRun::Sync(Box::new(move || {
                    from_disk_job_run(ctx, url, path)
                }))),
            )) {
                tracing::error!("{e}");
            }
        } else {
            let anim_path = self.path.clone();
            if let Err(e) = jobs.send(JobPackage::new(
                url.to_owned(),
                MediaJobKind::WebpImg,
                RunType::Output(JobRun::Async(Box::pin(from_net_run(
                    ctx, url, anim_path, imgtype,
                )))),
            )) {
                tracing::error!("{e}");
            }
        }

        &TextureState::<WebpCacheEntry>::Pending
    }
}

fn from_disk_job_run(ctx: egui::Context, url: String, path: PathBuf) -> JobOutput<MediaJobResult> {
    tracing::trace!("Starting webp from disk job for {url}");
    let webp_bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            return JobOutput::Complete(CompleteResponse::new(MediaJobResult::StaticImg(Err(
                Error::Io(e),
            ))))
        }
    };

    let webp_type = detect_webp_type(&webp_bytes);
    match webp_type {
        WebpType::Static => {
            tracing::trace!("Detected static webp for {url}");
            match decode_static_webp(&ctx, &url, &webp_bytes) {
                Ok(texture) => JobOutput::Complete(CompleteResponse::new(
                    MediaJobResult::StaticImg(Ok(texture)),
                )),
                Err(e) => {
                    JobOutput::Complete(CompleteResponse::new(MediaJobResult::StaticImg(Err(e))))
                }
            }
        }
        WebpType::Animated => {
            tracing::trace!("Detected animated webp for {url}");
            JobOutput::Complete(CompleteResponse::new(MediaJobResult::Animation(
                generate_webp_anim_pkg(ctx.clone(), url.to_owned(), webp_bytes.as_slice(), |img| {
                    buffer_to_color_image(img.as_flat_samples_u8(), img.width(), img.height())
                })
                .map(|f| f.anim),
            )))
        }
    }
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
            return JobOutput::complete(MediaJobResult::StaticImg(Err(crate::Error::Generic(
                format!("Http error: {e}"),
            ))));
        }
    };

    JobOutput::Next(JobRun::Sync(Box::new(move || {
        tracing::trace!("Starting webp from net job for {url}");

        let webp_type = detect_webp_type(&res.bytes);
        match webp_type {
            WebpType::Static => {
                tracing::trace!("Detected static webp from net for {url}");
                match decode_static_webp_processed(&ctx, &url, &res.bytes, imgtype) {
                    Ok(image) => {
                        let texture =
                            load_texture_checked(&ctx, &url, image.clone(), Default::default());

                        JobOutput::Complete(
                            CompleteResponse::new(MediaJobResult::StaticImg(Ok(texture)))
                                .run_no_output(NoOutputRun::Sync(Box::new(move || {
                                    tracing::trace!("writing static webp to file for {url}");
                                    if let Err(e) = MediaCache::write(&path, &url, image) {
                                        tracing::error!("Could not write static webp to disk: {e}");
                                    }
                                }))),
                        )
                    }
                    Err(e) => JobOutput::Complete(CompleteResponse::new(
                        MediaJobResult::StaticImg(Err(e)),
                    )),
                }
            }
            WebpType::Animated => {
                tracing::trace!("Detected animated webp from net for {url}");
                let animation = match generate_webp_anim_pkg(
                    ctx.clone(),
                    url.to_owned(),
                    &res.bytes,
                    move |img| process_image(imgtype, img),
                ) {
                    Ok(a) => a,
                    Err(e) => {
                        return JobOutput::Complete(CompleteResponse::new(
                            MediaJobResult::Animation(Err(e)),
                        ));
                    }
                };
                JobOutput::Complete(
                    CompleteResponse::new(MediaJobResult::Animation(Ok(animation.anim)))
                        .run_no_output(NoOutputRun::Sync(Box::new(move || {
                            tracing::trace!("writing animated webp texture to file for {url}");
                            if let Err(e) =
                                MediaCache::write_webp(&path, &url, animation.img_frames)
                            {
                                tracing::error!("Could not write webp to disk: {e}");
                            }
                        }))),
                )
            }
        }
    })))
}

fn generate_webp_anim_pkg(
    ctx: egui::Context,
    url: String,
    webp_bytes: &[u8],
    process_to_egui: impl Fn(DynamicImage) -> ColorImage + Send + Copy + 'static,
) -> Result<AnimationPackage, Error> {
    let decoded = match AnimDecoder::new(webp_bytes).decode() {
        Ok(image) => {
            if image.len() == 0 {
                return Err(crate::Error::Generic("No frames found in webp".to_owned()));
            }

            image
        }
        Err(e) => {
            return Err(crate::Error::Generic(format!(
                "Failed to decode webp frames: {e}"
            )))
        }
    };

    let Some(frames) = decoded.get_frames(0..decoded.len()) else {
        return Err(crate::Error::Generic(
            "Failed to iterate decoded webp frames".to_owned(),
        ));
    };

    let mut imgs = Vec::new();
    let mut other_frames = Vec::new();

    let mut first_frame = None;

    let mut prev_delay = Duration::from_millis(100);
    for (i, frame) in frames.iter().enumerate() {
        let delay = match frames.get(i + 1) {
            Some(next) => {
                let cur_ts = frame.get_time_ms().max(0) as u64;
                let next_ts = next.get_time_ms().max(0) as u64;
                // Avoiding zero-delay frames which can cause tight repaint loops.
                Duration::from_millis(next_ts.saturating_sub(cur_ts).max(1))
            }
            None => prev_delay,
        };

        let img = generate_color_img_frame(frame, process_to_egui);
        imgs.push(ImageFrame {
            delay,
            image: img.clone(),
        });

        let tex_frame = generate_animation_frame(&ctx, &url, i, delay.into(), img);

        if first_frame.is_none() {
            first_frame = Some(tex_frame);
        } else {
            other_frames.push(tex_frame);
        }

        prev_delay = delay;
    }

    let Some(first_frame) = first_frame else {
        return Err(crate::Error::Generic(
            "first frame not found for webp".to_owned(),
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
    frame: &webp::AnimFrame<'_>,
    process_to_egui: impl Fn(DynamicImage) -> ColorImage + Send + Copy + 'static,
) -> ColorImage {
    let width = frame.width();
    let height = frame.height();
    let data = frame.get_image();

    let dynamic_img = match frame.get_layout() {
        webp::PixelLayout::Rgb => match RgbImage::from_raw(width, height, data.to_vec()) {
            Some(img) => DynamicImage::ImageRgb8(img),
            None => {
                tracing::warn!("Failed to build RGB image from webp anim frame");
                DynamicImage::ImageRgba8(RgbaImage::from_pixel(1, 1, Rgba([0, 0, 0, 0])))
            }
        },
        webp::PixelLayout::Rgba => match RgbaImage::from_raw(width, height, data.to_vec()) {
            Some(img) => DynamicImage::ImageRgba8(img),
            None => {
                tracing::warn!("Failed to build RGBA image from webp anim frame");
                DynamicImage::ImageRgba8(RgbaImage::from_pixel(1, 1, Rgba([0, 0, 0, 0])))
            }
        },
    };

    process_to_egui(dynamic_img)
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
