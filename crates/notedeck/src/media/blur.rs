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
}

#[derive(Clone, Copy, Debug)]
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

/// Longest edge, in pixels, that a blurhash placeholder is decoded at.
///
/// A blurhash stores at most 9x9 DCT components, so whatever it decodes to is a
/// sum of cosines with no more than eight half-cycles across either axis. 64
/// samples per edge is seven per component, far past what bilinear
/// magnification needs to reproduce that. Measured against a full-resolution
/// decode of the same hash, a 64px decode magnified back up differs by a mean
/// of 1.1/255 per channel at the 9x9 worst case, and less below it.
///
/// It is a fixed cap rather than a function of the display size because the
/// display size is exactly what made these textures expensive: decoding at
/// `available_points * pixels_per_point` gave a full-width column image on a 2x
/// display a 3.7 MB texture (1280x720) to draw a blur with, and cost 3.4ms of
/// job-pool time to produce. 64px caps the texture at 16 KiB and the decode at
/// 0.02ms.
///
/// The placeholder is drawn magnified to the size of the media it stands in for,
/// which relies on the texture being uploaded with linear magnification — see
/// [`generate_blurhash_texturehandle`].
const BLUR_DECODE_MAX_EDGE: u32 = 64;

/// The ratio `x / y`, or `None` if it is not a usable aspect ratio.
fn aspect_ratio(x: f32, y: f32) -> Option<f32> {
    let ratio = x / y;
    (x > 0.0 && y > 0.0 && ratio.is_finite()).then_some(ratio)
}

impl ImageMetadata {
    /// The pixel dimensions to decode this blurhash at.
    ///
    /// Only the *shape* depends on the media: the size itself is always
    /// [`BLUR_DECODE_MAX_EDGE`] on the longer edge, independent of how large the
    /// placeholder will be drawn.
    ///
    /// The aspect ratio still has to be right, because the placeholder is laid
    /// out from its texture's aspect ratio and so should be the shape of the
    /// media it stands in for. That comes from the `imeta` `dim` tag; a
    /// blurhash on its own says nothing about the media's proportions.
    /// `available` — the space the media is being drawn into — is the fallback
    /// for imeta tags that carry no usable `dim`.
    pub fn blur_pixel_dimensions(&self, available: PointDimensions) -> PixelDimensions {
        let aspect = self
            .dimensions
            .and_then(|dim| aspect_ratio(dim.x as f32, dim.y as f32))
            .or_else(|| aspect_ratio(available.x, available.y))
            .unwrap_or(1.0);

        let edge = BLUR_DECODE_MAX_EDGE as f32;
        let (x, y) = if aspect >= 1.0 {
            (edge, edge / aspect)
        } else {
            (edge * aspect, edge)
        };

        // A degenerate aspect ratio (a `dim` like `4000x1`) would otherwise
        // round an axis down to zero, which is not a valid texture.
        PixelDimensions {
            x: (x.round() as u32).max(1),
            y: (y.round() as u32).max(1),
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

/// Decodes `blurhash` at `width` x `height` and uploads it as a texture.
///
/// The filtering is spelled out rather than left to `Default`: the placeholder
/// is decoded well below the size it is drawn at (see
/// [`BLUR_DECODE_MAX_EDGE`]), so linear *magnification* is what makes it look
/// like a blur instead of a grid of flat squares.
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
    Ok(load_texture_checked(
        ctx,
        url,
        img,
        egui::TextureOptions::LINEAR,
    ))
}

/// Blurhash placeholder textures, keyed by media URL.
///
/// Each is at most [`BLUR_DECODE_MAX_EDGE`] square, so they are cheap — but not
/// free, and there is one per piece of media ever scrolled past, so they are
/// still counted against the same budget as real images. See
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
    /// `available` is the space the media is being drawn into, and is used only
    /// as a fallback aspect ratio — see
    /// [`ImageMetadata::blur_pixel_dimensions`]. The decode size deliberately
    /// does not scale with it.
    ///
    /// Yields the texture rather than the whole `&BlurState` because there is
    /// no `BlurState` to point at on a miss: [`TexEntry`] has interior
    /// mutability, so a `Pending` placeholder cannot be promoted to a `'static`
    /// the way the image caches' `&TextureState::Pending` is.
    pub fn get_or_request(
        &self,
        jobs: &MediaJobSender,
        ctx: &egui::Context,
        url: &str,
        blurhash: &ImageMetadata,
        available: PointDimensions,
    ) -> Option<&TextureHandle> {
        if let Some(res) = self.get(url, ctx.cumulative_pass_nr()) {
            return match res.tex_state() {
                TextureState::Loaded(texture) => Some(texture),
                TextureState::Pending | TextureState::Error(_) => None,
            };
        }

        let pixel_sizes = blurhash.blur_pixel_dimensions(available);
        let blurhash = blurhash.blurhash.to_owned();
        let url = url.to_owned();
        let ctx = ctx.clone();

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A real 4x3-component blurhash (the `blurhash` crate's own octocat
    /// fixture), which has near-black regions and so exaggerates any
    /// reconstruction error once the decode is converted back to sRGB.
    const OCTOCAT: &str = "LNAdAqj[00aymkj[TKay9}ay-Sj[";

    fn meta(dim: Option<(u32, u32)>) -> ImageMetadata {
        ImageMetadata {
            blurhash: OCTOCAT.to_owned(),
            dimensions: dim.map(|(x, y)| PixelDimensions { x, y }),
        }
    }

    fn points(x: f32, y: f32) -> PointDimensions {
        PointDimensions { x, y }
    }

    #[test]
    fn decode_size_is_capped_and_shaped_by_the_dim_tag() {
        let dims = meta(Some((1920, 1080))).blur_pixel_dimensions(points(400.0, 360.0));

        assert_eq!(dims.x, BLUR_DECODE_MAX_EDGE);
        assert_eq!(dims.y, 36, "16:9 kept, long edge capped");

        // Portrait caps the other axis.
        let dims = meta(Some((1080, 1920))).blur_pixel_dimensions(points(400.0, 360.0));
        assert_eq!((dims.x, dims.y), (36, BLUR_DECODE_MAX_EDGE));
    }

    #[test]
    fn decode_size_does_not_scale_with_the_display() {
        let meta = meta(Some((1920, 1080)));

        // The whole point of the cap: a huge slot on a hidpi display must not
        // mint a bigger placeholder than a small slot on a 1x one.
        let small_slot = meta.blur_pixel_dimensions(points(64.0, 64.0));
        let full_column = meta.blur_pixel_dimensions(points(600.0, 360.0));
        let fullscreen = meta.blur_pixel_dimensions(points(3840.0, 2160.0));

        assert_eq!((small_slot.x, small_slot.y), (full_column.x, full_column.y));
        assert_eq!((small_slot.x, small_slot.y), (fullscreen.x, fullscreen.y));
    }

    #[test]
    fn a_blurhash_alone_takes_its_shape_from_the_available_space() {
        // No `dim` tag: nothing but the space it is being drawn into says what
        // shape the media is.
        let dims = meta(None).blur_pixel_dimensions(points(400.0, 200.0));
        assert_eq!((dims.x, dims.y), (BLUR_DECODE_MAX_EDGE, 32));
    }

    #[test]
    fn unusable_dimensions_fall_back_rather_than_producing_an_invalid_texture() {
        // Relays hand us whatever the poster's client wrote, so a zero, a
        // degenerate ratio, or an unbounded slot all have to land somewhere
        // valid: a texture axis of zero would panic on upload.
        for (dim, slot) in [
            (Some((0, 0)), points(400.0, 200.0)),
            (Some((1920, 0)), points(400.0, 200.0)),
            (None, points(400.0, 0.0)),
            (None, points(f32::INFINITY, 200.0)),
            (None, points(f32::NAN, f32::NAN)),
            (Some((4000, 1)), points(400.0, 200.0)),
        ] {
            let dims = meta(dim).blur_pixel_dimensions(slot);
            assert!(
                dims.x >= 1 && dims.y >= 1,
                "{dim:?} in {slot:?} gave {dims:?}"
            );
            assert!(
                dims.x <= BLUR_DECODE_MAX_EDGE && dims.y <= BLUR_DECODE_MAX_EDGE,
                "{dim:?} in {slot:?} gave {dims:?}"
            );
        }
    }

    #[test]
    fn a_blur_texture_costs_kilobytes_not_megabytes() {
        // epaint's texture manager is pure CPU, so this measures a real
        // TextureHandle without needing a GPU.
        let ctx = egui::Context::default();
        let meta = meta(Some((1920, 1080)));
        let dims = meta.blur_pixel_dimensions(points(600.0, 360.0));

        let tex = generate_blurhash_texturehandle(&ctx, &meta.blurhash, "url", dims.x, dims.y)
            .expect("decodes");

        assert_eq!(tex.byte_size(), 64 * 36 * 4);

        // And the budget must bill that size, not the size of the media the
        // placeholder stands in for: TexEntry samples byte_size at insert.
        let entry = TexEntry::new(TextureState::Loaded(tex), 0);
        assert_eq!(entry.bytes(), 64 * 36 * 4);
    }

    /// Magnifies `src` to `dst_w` x `dst_h` the way a GPU sampler with linear
    /// filtering and clamp-to-edge does: bilinear between texel centers.
    fn magnify_bilinear(
        src: &[u8],
        src_w: usize,
        src_h: usize,
        dst_w: usize,
        dst_h: usize,
    ) -> Vec<u8> {
        let mut out = vec![0u8; dst_w * dst_h * 4];
        let sample = |x: usize, y: usize, c: usize| src[(y * src_w + x) * 4 + c] as f32;

        for dy in 0..dst_h {
            let fy = ((dy as f32 + 0.5) * src_h as f32 / dst_h as f32 - 0.5)
                .clamp(0.0, src_h as f32 - 1.0);
            let (y0, wy) = (fy.floor() as usize, fy.fract());
            let y1 = (y0 + 1).min(src_h - 1);

            for dx in 0..dst_w {
                let fx = ((dx as f32 + 0.5) * src_w as f32 / dst_w as f32 - 0.5)
                    .clamp(0.0, src_w as f32 - 1.0);
                let (x0, wx) = (fx.floor() as usize, fx.fract());
                let x1 = (x0 + 1).min(src_w - 1);

                for c in 0..4 {
                    let top = sample(x0, y0, c) * (1.0 - wx) + sample(x1, y0, c) * wx;
                    let bot = sample(x0, y1, c) * (1.0 - wx) + sample(x1, y1, c) * wx;
                    out[(dy * dst_w + dx) * 4 + c] = (top * (1.0 - wy) + bot * wy).round() as u8;
                }
            }
        }

        out
    }

    #[test]
    fn a_capped_decode_magnifies_back_to_what_a_full_decode_would_have_drawn() {
        // The claim the cap rests on: a blurhash carries so few components that
        // sampling it at BLUR_DECODE_MAX_EDGE and letting the GPU magnify is
        // the same picture as decoding it at the size it is drawn at.
        let (full_w, full_h) = (800u32, 600u32);
        let reference = blurhash::decode(OCTOCAT, full_w, full_h, 1.0).expect("decodes");

        let dims = meta(Some((full_w, full_h))).blur_pixel_dimensions(points(400.0, 300.0));
        let small = blurhash::decode(OCTOCAT, dims.x, dims.y, 1.0).expect("decodes");
        let magnified = magnify_bilinear(
            &small,
            dims.x as usize,
            dims.y as usize,
            full_w as usize,
            full_h as usize,
        );

        let mut peak = 0u32;
        let mut total = 0u64;
        for (got, want) in magnified.iter().zip(reference.iter()) {
            let err = (*got as i32 - *want as i32).unsigned_abs();
            peak = peak.max(err);
            total += err as u64;
        }
        let mean = total as f64 / reference.len() as f64;

        // Loose enough that the exact rounding of a real sampler does not
        // matter, tight enough to fail if the cap drops far enough to start
        // losing components: a 16px decode of this hash is mean 6.5, peak 51.
        assert!(mean < 4.0, "mean error {mean}/255 per channel");
        assert!(peak < 45, "peak error {peak}/255");
    }
}
