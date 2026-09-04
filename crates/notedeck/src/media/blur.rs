use std::collections::HashMap;

use egui::{TextureHandle, Vec2};
use nostrdb::Note;

use crate::{
    jobs::{
        CompleteResponse, JobOutput, JobPackage, JobRun, MediaJobKind, MediaJobResult,
        MediaJobSender, RunType,
    },
    media::budget::{EvictCandidate, TexEntry},
    media::load_texture_checked,
    TextureState,
};

#[derive(Clone)]
pub struct ImageMetadata {
    pub blurhash: String,
    pub dimensions: Option<PixelDimensions>, // width and height in pixels
}

#[derive(Clone, Debug, Copy, PartialEq, Eq, Hash)]
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

    /// Rounds each axis up to the nearest multiple of `step`.
    pub fn snap(self, step: u32) -> Self {
        if step == 0 {
            return self;
        }
        Self {
            x: self.x.div_ceil(step) * step,
            y: self.y.div_ceil(step) * step,
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
    pub fn from_vec(vec: Vec2) -> Self {
        Self { x: vec.x, y: vec.y }
    }

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
            return *defined_dimensions;
        }

        let scale_factor = (max_pixels.y as f32) / (defined_dimensions.y as f32);
        let max_width_scaled = scale_factor * (defined_dimensions.x as f32);

        PixelDimensions {
            x: (max_width_scaled.round() as u32),
            y: max_pixels.y,
        }
    }
}

/// Find blurhashes in image metadata and update our cache
pub fn update_imeta_blurhashes(note: &Note, blurs: &mut HashMap<String, ImageMetadata>) {
    for tag in note.tags() {
        let mut tag_iter = tag.into_iter();
        if tag_iter
            .next()
            .and_then(|s| s.str())
            .filter(|s| *s == "imeta")
            .is_none()
        {
            continue;
        }

        let Some((url, blur)) = find_blur(tag_iter) else {
            continue;
        };

        blurs.insert(url.to_string(), blur);
    }
}

fn find_blur(tag_iter: nostrdb::TagIter<'_>) -> Option<(String, ImageMetadata)> {
    let mut url = None;
    let mut blurhash = None;
    let mut dims = None;

    for tag_elem in tag_iter {
        let Some(s) = tag_elem.str() else { continue };
        let mut split = s.split_whitespace();

        let Some(first) = split.next() else { continue };
        let Some(second) = split.next() else { continue };

        match first {
            "url" => url = Some(second),
            "blurhash" => blurhash = Some(second),
            "dim" => dims = Some(second),
            _ => {}
        }

        if url.is_some() && blurhash.is_some() && dims.is_some() {
            break;
        }
    }

    let url = url?;
    let blurhash = blurhash?;

    let dimensions = dims.and_then(|d| {
        let mut split = d.split('x');
        let width = split.next()?.parse::<u32>().ok()?;
        let height = split.next()?.parse::<u32>().ok()?;

        Some(PixelDimensions {
            x: width,
            y: height,
        })
    });

    Some((
        url.to_string(),
        ImageMetadata {
            blurhash: blurhash.to_string(),
            dimensions,
        },
    ))
}

#[derive(Clone)]
pub enum ObfuscationType {
    Blurhash(ImageMetadata),
    Default,
}

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

/// Blurhash placeholder textures, keyed by media URL.
///
/// These are not free: [`ImageMetadata::scaled_pixel_dimensions`] decodes a
/// blurhash at the display size of the media it stands in for, so a full-width
/// column image on a 2x display produces a multi-megabyte texture. They are
/// therefore counted against the same budget as real images — see
/// [`crate::media::budget`].
#[derive(Default)]
pub struct BlurCache {
    cache: HashMap<String, BlurState>,

    /// Running total of the bytes held by loaded entries.
    loaded_bytes: usize,
}

pub struct BlurState {
    entry: TexEntry<TextureHandle>,
    pub finished_transitioning: bool,
}

impl BlurState {
    /// The blur texture's load state.
    ///
    /// Reading this does not count as a use; [`BlurCache::get`] already
    /// recorded one.
    pub fn tex_state(&self) -> &TextureState<TextureHandle> {
        self.entry.peek()
    }
}

impl BlurCache {
    /// Reads the blur state for `url`, recording it as used during `pass_nr`.
    pub fn get(&self, url: &str, pass_nr: u64) -> Option<&BlurState> {
        let state = self.cache.get(url)?;
        state.entry.touch(pass_nr);
        Some(state)
    }

    /// Returns the blur texture for `url`, dispatching a blurhash decode job if
    /// it is not cached yet.
    ///
    /// Yields the texture rather than the whole `&BlurState` because there is
    /// no `BlurState` to point at on a miss: [`TexEntry`] has interior
    /// mutability, so a `Pending` placeholder cannot be promoted to a `'static`
    /// the way the image caches' `&TextureState::Pending` is.
    pub fn get_or_request(
        &self,
        jobs: &MediaJobSender,
        ui: &egui::Ui,
        url: &str,
        blurhash: &ImageMetadata,
        size: egui::Vec2,
    ) -> Option<&TextureHandle> {
        if let Some(res) = self.get(url, ui.ctx().cumulative_pass_nr()) {
            return match res.tex_state() {
                TextureState::Loaded(texture) => Some(texture),
                TextureState::Pending | TextureState::Error(_) => None,
            };
        }

        let available_points = PointDimensions {
            x: size.x,
            y: size.y,
        };
        let pixel_sizes = blurhash.scaled_pixel_dimensions(ui, available_points);
        let blurhash = blurhash.blurhash.to_owned();
        let url = url.to_owned();
        let ctx = ui.ctx().clone();

        if let Err(e) = jobs.send(JobPackage::new(
            url.to_owned(),
            MediaJobKind::Blurhash,
            RunType::Output(JobRun::Sync(Box::new(move || {
                tracing::trace!("Starting blur job for {url}");
                let res = generate_blurhash_texturehandle(
                    &ctx,
                    &blurhash,
                    &url,
                    pixel_sizes.x,
                    pixel_sizes.y,
                );
                JobOutput::Complete(CompleteResponse::new(MediaJobResult::Blurhash(res)))
            }))),
        )) {
            tracing::error!("{e}");
        }

        None
    }

    pub fn finished_transitioning(&mut self, url: &str) {
        let Some(state) = self.cache.get_mut(url) else {
            return;
        };

        state.finished_transitioning = true;
    }

    /// Stores the outcome of a blurhash job for `url`.
    pub fn set_state(&mut self, url: String, state: TextureState<TextureHandle>, pass_nr: u64) {
        let entry = TexEntry::new(state, pass_nr);
        self.loaded_bytes += entry.bytes();

        let replaced = self.cache.insert(
            url,
            BlurState {
                entry,
                finished_transitioning: false,
            },
        );

        if let Some(replaced) = replaced {
            self.loaded_bytes = self.loaded_bytes.saturating_sub(replaced.entry.bytes());
        }
    }

    /// GPU bytes currently held by loaded blur textures.
    pub fn loaded_bytes(&self) -> usize {
        self.loaded_bytes
    }

    /// Number of loaded blur textures, for diagnostics.
    pub fn loaded_count(&self) -> usize {
        self.cache
            .values()
            .filter(|state| matches!(state.tex_state(), TextureState::Loaded(_)))
            .count()
    }

    /// Appends every entry a sweep of `current_pass` could drop. See
    /// [`crate::media::budget::VariantTexCache::collect_evictable`].
    pub fn collect_evictable(&self, current_pass: u64, out: &mut Vec<EvictCandidate>) {
        for state in self.cache.values() {
            if let Some(candidate) = state.entry.as_candidate(current_pass) {
                out.push(candidate);
            }
        }
    }

    /// Drops evictable entries last used at or before `cutoff_pass`, stopping
    /// once `to_free` bytes have been released. Returns the bytes freed.
    ///
    /// Unlike the image caches this drops the whole entry, including
    /// `finished_transitioning`: a blur only renders while its media is still
    /// loading, so if the media comes back it should shimmer in again.
    pub fn evict_until(&mut self, current_pass: u64, cutoff_pass: u64, to_free: usize) -> usize {
        let mut freed = 0;

        self.cache.retain(|_url, state| {
            if freed >= to_free {
                return true;
            }
            let Some(candidate) = state.entry.as_candidate(current_pass) else {
                return true;
            };
            if candidate.last_used > cutoff_pass {
                return true;
            }

            freed += candidate.bytes;
            false
        });

        self.loaded_bytes = self.loaded_bytes.saturating_sub(freed);
        freed
    }

    /// Drops every entry and its textures.
    pub fn clear(&mut self) {
        self.cache.clear();
        self.loaded_bytes = 0;
    }
}
