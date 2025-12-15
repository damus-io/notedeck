use std::collections::HashMap;

use base64::prelude::*;
use egui::TextureHandle;
use nostrdb::Note;

use crate::{
    jobs::{
        CompleteResponse, JobOutput, JobPackage, JobRun, MediaJobKind, MediaJobResult,
        MediaJobSender, RunType,
    },
    media::load_texture_checked,
    TextureState,
};

/// Represents the type of placeholder hash available for an image.
/// Thumbhash is preferred when available as it provides better quality
/// and includes embedded aspect ratio information.
#[derive(Clone, Debug)]
pub enum PlaceholderHash {
    /// Thumbhash binary data (more modern, better quality)
    ThumbHash(Vec<u8>),
    /// Blurhash string (widely supported fallback)
    BlurHash(String),
}

#[derive(Clone, Debug)]
pub struct ImageMetadata {
    /// The placeholder hash for this image (thumbhash or blurhash)
    pub hash: PlaceholderHash,
    /// Original image dimensions in pixels (used for aspect ratio)
    pub dimensions: Option<PixelDimensions>,
}

#[derive(Clone, Debug)]
pub struct PixelDimensions {
    pub x: u32,
    pub y: u32,
}

impl PixelDimensions {
    pub fn to_points(&self, ppp: f32) -> PointDimensions {
        PointDimensions {
            x: (self.x as f32) / ppp,
            y: (self.y as f32) / ppp,
        }
    }

    pub fn clamp_wgpu(mut self) -> PixelDimensions {
        let val = super::MAX_SIZE_WGPU as u32;
        if self.x > val {
            self.x = val;
        }

        if self.y > val {
            self.y = val
        }

        self
    }
}

#[derive(Clone, Debug)]
pub struct PointDimensions {
    pub x: f32,
    pub y: f32,
}

impl PointDimensions {
    pub fn to_pixels(self, ui: &egui::Ui) -> PixelDimensions {
        PixelDimensions {
            x: (self.x * ui.pixels_per_point()).round() as u32,
            y: (self.y * ui.pixels_per_point()).round() as u32,
        }
    }

    pub fn to_vec(self) -> egui::Vec2 {
        egui::Vec2::new(self.x, self.y)
    }
}

impl ImageMetadata {
    pub fn scaled_pixel_dimensions(
        &self,
        ui: &egui::Ui,
        available_points: PointDimensions,
    ) -> PixelDimensions {
        let max_pixels = available_points.to_pixels(ui).clamp_wgpu();

        let Some(defined_dimensions) = &self.dimensions else {
            return max_pixels;
        };

        if defined_dimensions.x == 0 || defined_dimensions.y == 0 {
            tracing::error!("The blur dimensions should not be zero");
            return max_pixels;
        }

        if defined_dimensions.y <= max_pixels.y {
            return defined_dimensions.clone();
        }

        let scale_factor = (max_pixels.y as f32) / (defined_dimensions.y as f32);
        let max_width_scaled = scale_factor * (defined_dimensions.x as f32);

        PixelDimensions {
            x: (max_width_scaled.round() as u32),
            y: max_pixels.y,
        }
    }
}

/// Extract placeholder hashes (thumbhash or blurhash) from note imeta tags.
/// Thumbhash is preferred over blurhash when both are present.
pub fn update_imeta_placeholders(note: &Note, metadata: &mut HashMap<String, ImageMetadata>) {
    for tag in note.tags() {
        let mut tag_iter = tag.into_iter();

        // Check if this is an imeta tag
        let is_imeta = tag_iter
            .next()
            .and_then(|s| s.str())
            .is_some_and(|s| s == "imeta");

        if !is_imeta {
            continue;
        }

        let Some((url, meta)) = find_placeholder(tag_iter) else {
            continue;
        };

        metadata.insert(url, meta);
    }
}

/// Parse an imeta tag to extract URL and placeholder hash.
/// Prefers thumbhash over blurhash when both are available.
fn find_placeholder(tag_iter: nostrdb::TagIter<'_>) -> Option<(String, ImageMetadata)> {
    let mut url = None;
    let mut blurhash = None;
    let mut thumbhash = None;
    let mut dims = None;

    for tag_elem in tag_iter {
        let Some(s) = tag_elem.str() else { continue };
        let mut split = s.split_whitespace();

        let Some(key) = split.next() else { continue };
        let Some(value) = split.next() else { continue };

        match key {
            "url" => url = Some(value.to_string()),
            "blurhash" => blurhash = Some(value.to_string()),
            "thumbhash" => thumbhash = Some(value.to_string()),
            "dim" => dims = Some(value),
            _ => {}
        }
    }

    let url = url?;

    // Prefer thumbhash over blurhash (better quality, includes aspect ratio)
    let hash = if let Some(th) = thumbhash {
        // Thumbhash in imeta tags is base64 encoded
        let decoded = BASE64_STANDARD.decode(th).ok()?;
        PlaceholderHash::ThumbHash(decoded)
    } else if let Some(bh) = blurhash {
        PlaceholderHash::BlurHash(bh)
    } else {
        // No placeholder hash available
        return None;
    };

    let dimensions = dims.and_then(|d| {
        let mut split = d.split('x');
        let width = split.next()?.parse::<u32>().ok()?;
        let height = split.next()?.parse::<u32>().ok()?;
        Some(PixelDimensions {
            x: width,
            y: height,
        })
    });

    Some((url, ImageMetadata { hash, dimensions }))
}

/// Specifies how an image should be obfuscated while loading or for untrusted content.
/// ThumbHash is preferred over Blurhash when available.
#[derive(Clone)]
pub enum ObfuscationType {
    /// Use thumbhash placeholder (preferred - better quality)
    ThumbHash(ImageMetadata),
    /// Use blurhash placeholder (fallback for compatibility)
    Blurhash(ImageMetadata),
    /// Use default solid color placeholder
    Default,
}

/// Decode a blurhash string into an egui texture at the specified dimensions.
fn generate_blurhash_texturehandle(
    ctx: &egui::Context,
    blurhash: &str,
    url: &str,
    width: u32,
    height: u32,
) -> Result<egui::TextureHandle, crate::Error> {
    let bytes = blurhash::decode(blurhash, width, height, 1.0)
        .map_err(|e| crate::Error::Generic(e.to_string()))?;

    let img = egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &bytes);
    Ok(load_texture_checked(ctx, url, img, Default::default()))
}

/// Decode a thumbhash into an egui texture.
/// Thumbhash automatically determines output dimensions from the hash itself.
fn generate_thumbhash_texturehandle(
    ctx: &egui::Context,
    thumbhash: &[u8],
    url: &str,
) -> Result<egui::TextureHandle, crate::Error> {
    let (width, height, rgba) = thumbhash::thumb_hash_to_rgba(thumbhash)
        .map_err(|_| crate::Error::Generic("thumbhash decode failed".to_string()))?;

    let img = egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba);
    Ok(load_texture_checked(ctx, url, img, Default::default()))
}

#[derive(Default)]
pub struct BlurCache {
    pub(crate) cache: HashMap<String, BlurState>,
}

pub struct BlurState {
    pub tex_state: TextureState<TextureHandle>,
    pub finished_transitioning: bool,
}

impl From<TextureState<TextureHandle>> for BlurState {
    fn from(value: TextureState<TextureHandle>) -> Self {
        BlurState {
            tex_state: value,
            finished_transitioning: false,
        }
    }
}

impl BlurCache {
    pub fn get(&self, url: &str) -> Option<&BlurState> {
        self.cache.get(url)
    }

    /// Get a cached blur texture or request its generation.
    /// Handles both thumbhash and blurhash placeholder types.
    pub fn get_or_request(
        &self,
        jobs: &MediaJobSender,
        ui: &egui::Ui,
        url: &str,
        metadata: &ImageMetadata,
        size: egui::Vec2,
    ) -> &BlurState {
        if let Some(res) = self.cache.get(url) {
            return res;
        }

        let url_owned = url.to_owned();
        let ctx = ui.ctx().clone();

        // Dispatch based on placeholder hash type
        match &metadata.hash {
            PlaceholderHash::ThumbHash(data) => {
                self.request_thumbhash_job(jobs, &ctx, &url_owned, data.clone());
            }
            PlaceholderHash::BlurHash(hash) => {
                let available_points = PointDimensions {
                    x: size.x,
                    y: size.y,
                };
                let pixel_sizes = metadata.scaled_pixel_dimensions(ui, available_points);
                self.request_blurhash_job(jobs, &ctx, &url_owned, hash.clone(), pixel_sizes);
            }
        }

        &BlurState {
            tex_state: TextureState::Pending,
            finished_transitioning: false,
        }
    }

    /// Request a thumbhash decoding job.
    fn request_thumbhash_job(
        &self,
        jobs: &MediaJobSender,
        ctx: &egui::Context,
        url: &str,
        data: Vec<u8>,
    ) {
        let url = url.to_owned();
        let ctx = ctx.clone();

        if let Err(e) = jobs.send(JobPackage::new(
            url.clone(),
            MediaJobKind::ThumbHash,
            RunType::Output(JobRun::Sync(Box::new(move || {
                tracing::trace!("Starting thumbhash job for {url}");
                let res = generate_thumbhash_texturehandle(&ctx, &data, &url);
                JobOutput::Complete(CompleteResponse::new(MediaJobResult::ThumbHash(res)))
            }))),
        )) {
            tracing::error!("{e}");
        }
    }

    /// Request a blurhash decoding job.
    fn request_blurhash_job(
        &self,
        jobs: &MediaJobSender,
        ctx: &egui::Context,
        url: &str,
        hash: String,
        pixel_sizes: PixelDimensions,
    ) {
        let url = url.to_owned();
        let ctx = ctx.clone();

        if let Err(e) = jobs.send(JobPackage::new(
            url.clone(),
            MediaJobKind::Blurhash,
            RunType::Output(JobRun::Sync(Box::new(move || {
                tracing::trace!("Starting blurhash job for {url}");
                let res = generate_blurhash_texturehandle(
                    &ctx,
                    &hash,
                    &url,
                    pixel_sizes.x,
                    pixel_sizes.y,
                );
                JobOutput::Complete(CompleteResponse::new(MediaJobResult::Blurhash(res)))
            }))),
        )) {
            tracing::error!("{e}");
        }
    }

    pub fn finished_transitioning(&mut self, url: &str) {
        let Some(state) = self.cache.get_mut(url) else {
            return;
        };

        state.finished_transitioning = true;
    }
}
