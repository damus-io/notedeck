use crate::media::network::HyperHttpResponse;
use crate::PixelDimensions;
use egui::{pos2, Color32, ColorImage, Rect, Sense, SizeHint};
use hashbrown::{Equivalent, HashMap};
use image::imageops::FilterType;
use image::FlatSamples;
use std::path::PathBuf;

// NOTE(jb55): chatgpt wrote this because I was too dumb to
pub fn aspect_fill(
    ui: &mut egui::Ui,
    sense: Sense,
    texture_id: egui::TextureId,
    aspect_ratio: f32,
) -> egui::Response {
    let frame = ui.available_rect_before_wrap(); // Get the available frame space in the current layout
    let frame_ratio = frame.width() / frame.height();

    let (width, height) = if frame_ratio > aspect_ratio {
        // Frame is wider than the content
        (frame.width(), frame.width() / aspect_ratio)
    } else {
        // Frame is taller than the content
        (frame.height() * aspect_ratio, frame.height())
    };

    let content_rect = Rect::from_min_size(
        frame.min
            + egui::vec2(
                (frame.width() - width) / 2.0,
                (frame.height() - height) / 2.0,
            ),
        egui::vec2(width, height),
    );

    // Set the clipping rectangle to the frame
    //let clip_rect = ui.clip_rect(); // Preserve the original clipping rectangle
    //ui.set_clip_rect(frame);

    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));

    let (response, painter) = ui.allocate_painter(ui.available_size(), sense);

    // Draw the texture within the calculated rect, potentially clipping it
    painter.rect_filled(content_rect, 0.0, ui.ctx().style().visuals.window_fill());
    painter.image(texture_id, content_rect, uv, Color32::WHITE);

    // Restore the original clipping rectangle
    //ui.set_clip_rect(clip_rect);
    response
}

#[profiling::function]
pub fn round_image(image: &mut ColorImage) {
    // The radius to the edge of of the avatar circle
    let edge_radius = image.size[0] as f32 / 2.0;
    let edge_radius_squared = edge_radius * edge_radius;

    for (pixnum, pixel) in image.pixels.iter_mut().enumerate() {
        // y coordinate
        let uy = pixnum / image.size[0];
        let y = uy as f32;
        let y_offset = edge_radius - y;

        // x coordinate
        let ux = pixnum % image.size[0];
        let x = ux as f32;
        let x_offset = edge_radius - x;

        // The radius to this pixel (may be inside or outside the circle)
        let pixel_radius_squared: f32 = x_offset * x_offset + y_offset * y_offset;

        // If inside of the avatar circle
        if pixel_radius_squared <= edge_radius_squared {
            // squareroot to find how many pixels we are from the edge
            let pixel_radius: f32 = pixel_radius_squared.sqrt();
            let distance = edge_radius - pixel_radius;

            // If we are within 1 pixel of the edge, we should fade, to
            // antialias the edge of the circle. 1 pixel from the edge should
            // be 100% of the original color, and right on the edge should be
            // 0% of the original color.
            if distance <= 1.0 {
                *pixel = Color32::from_rgba_premultiplied(
                    (pixel.r() as f32 * distance) as u8,
                    (pixel.g() as f32 * distance) as u8,
                    (pixel.b() as f32 * distance) as u8,
                    (pixel.a() as f32 * distance) as u8,
                );
            }
        } else {
            // Outside of the avatar circle
            *pixel = Color32::TRANSPARENT;
        }
    }
}

/// If the image's longest dimension is greater than max_edge, downscale
fn resize_image_if_too_big(
    image: image::DynamicImage,
    max_edge: u32,
    filter: FilterType,
) -> image::DynamicImage {
    // if we have no size hint, resize to something reasonable
    let w = image.width();
    let h = image.height();
    let long = w.max(h);

    if long > max_edge {
        let scale = max_edge as f32 / long as f32;
        let new_w = (w as f32 * scale).round() as u32;
        let new_h = (h as f32 * scale).round() as u32;

        image.resize(new_w, new_h, filter)
    } else {
        image
    }
}

///
/// Process an image, resizing so we don't blow up video memory or even crash
///
/// For profile pictures, make them round and small to fit the size hint
/// For everything else, either:
///
///   - resize to the size hint
///   - keep the size if the longest dimension is less than MAX_IMG_LENGTH
///   - resize if any larger, using [`resize_image_if_too_big`]
///
#[profiling::function]
pub fn process_image(imgtyp: ImageType, mut image: image::DynamicImage) -> ColorImage {
    const MAX_IMG_LENGTH: u32 = 2048;
    const FILTER_TYPE: FilterType = FilterType::CatmullRom;

    match imgtyp {
        ImageType::Content(size_hint) => {
            let image = match size_hint {
                None => resize_image_if_too_big(image, MAX_IMG_LENGTH, FILTER_TYPE),
                Some(pixels) => image.resize(pixels.x, pixels.y, FILTER_TYPE),
            };

            let image_buffer = image.into_rgba8();
            ColorImage::from_rgba_unmultiplied(
                [
                    image_buffer.width() as usize,
                    image_buffer.height() as usize,
                ],
                image_buffer.as_flat_samples().as_slice(),
            )
        }
        ImageType::Profile(size) => {
            // Crop square
            let smaller = image.width().min(image.height());

            if image.width() > smaller {
                let excess = image.width() - smaller;
                image = image.crop_imm(excess / 2, 0, image.width() - excess, image.height());
            } else if image.height() > smaller {
                let excess = image.height() - smaller;
                image = image.crop_imm(0, excess / 2, image.width(), image.height() - excess);
            }
            let image = image.resize(size, size, FilterType::CatmullRom); // DynamicImage
            let image_buffer = image.into_rgba8(); // RgbaImage (ImageBuffer)
            let mut color_image = ColorImage::from_rgba_unmultiplied(
                [
                    image_buffer.width() as usize,
                    image_buffer.height() as usize,
                ],
                image_buffer.as_flat_samples().as_slice(),
            );
            round_image(&mut color_image);
            color_image
        }
    }
}

#[profiling::function]
pub fn parse_img_response(
    response: HyperHttpResponse,
    imgtyp: ImageType,
) -> Result<ColorImage, crate::Error> {
    let content_type = response.content_type.unwrap_or_default();
    let size_hint = match imgtyp {
        ImageType::Profile(size) => SizeHint::Size(size, size),
        ImageType::Content(Some(pixels)) => SizeHint::Size(pixels.x, pixels.y),
        ImageType::Content(None) => SizeHint::default(),
    };

    if content_type.starts_with("image/svg") {
        profiling::scope!("load_svg");

        let mut color_image =
            egui_extras::image::load_svg_bytes_with_size(&response.bytes, Some(size_hint))?;
        round_image(&mut color_image);
        Ok(color_image)
    } else if content_type.starts_with("image/") {
        profiling::scope!("load_from_memory");
        let dyn_image = image::load_from_memory(&response.bytes)?;
        Ok(process_image(imgtyp, dyn_image))
    } else {
        Err(format!("Expected image, found content-type {content_type:?}").into())
    }
}

pub fn buffer_to_color_image(
    samples: Option<FlatSamples<&[u8]>>,
    width: u32,
    height: u32,
) -> ColorImage {
    // TODO(jb55): remove unwrap here
    let flat_samples = samples.unwrap();
    ColorImage::from_rgba_unmultiplied([width as usize, height as usize], flat_samples.as_slice())
}

pub fn fetch_binary_from_disk(path: PathBuf) -> Result<Vec<u8>, crate::Error> {
    std::fs::read(path).map_err(|e| crate::Error::Generic(e.to_string()))
}

/// Prefix delimiter used in request keys so one URL can have multiple cached texture variants.
const REQUEST_KEY_DELIMITER: &str = "::";

/// A strongly typed texture request identity used by in-memory texture caches.
///
/// The string form is only used at the jobs boundary (`JobPackage` IDs).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TextureRequestKey {
    pub url: String,
    pub variant: TextureRequestVariant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureRequestVariant {
    Full,
    Hint(PixelDimensions),
    Profile(u32),
}

impl TextureRequestKey {
    /// Build the request variant from the requested image type.
    pub fn variant_for_image_type(img_type: ImageType) -> TextureRequestVariant {
        match img_type {
            ImageType::Profile(size) => TextureRequestVariant::Profile(size),
            ImageType::Content(Some(pixels)) => TextureRequestVariant::Hint(pixels),
            ImageType::Content(None) => TextureRequestVariant::Full,
        }
    }

    /// Build a typed request key from a URL and request variant.
    pub fn from_variant(url: &str, variant: TextureRequestVariant) -> Self {
        Self {
            url: url.to_owned(),
            variant,
        }
    }

    /// Encode this typed key into a stable jobs-compatible string ID.
    pub fn to_job_id(&self) -> String {
        let suffix = match self.variant {
            TextureRequestVariant::Profile(size) => format!("profile-{size}"),
            TextureRequestVariant::Hint(pixels) => format!("hint-{}x{}", pixels.x, pixels.y),
            TextureRequestVariant::Full => "full".to_owned(),
        };
        format!("{}{REQUEST_KEY_DELIMITER}{suffix}", self.url)
    }
}

#[derive(Hash, PartialEq, Eq)]
struct TextureRequestLookup<'a> {
    url: &'a str,
    variant: TextureRequestVariant,
}

impl Equivalent<TextureRequestKey> for TextureRequestLookup<'_> {
    fn equivalent(&self, key: &TextureRequestKey) -> bool {
        key.url == self.url && key.variant == self.variant
    }
}

/// Borrowed lookup for texture request maps keyed by `(url, variant)`.
///
/// This avoids building an owned [`TextureRequestKey`] on cache hits.
pub fn get_cached_request_state<'a, V>(
    cache: &'a HashMap<TextureRequestKey, V>,
    url: &str,
    variant: TextureRequestVariant,
) -> Option<&'a V> {
    let lookup = TextureRequestLookup { url, variant };
    cache.get(&lookup)
}

/// Hint-sized content should still persist full-resolution data to disk.
pub fn should_persist_full_content(img_type: ImageType) -> bool {
    matches!(img_type, ImageType::Content(Some(_)))
}

/// Controls type-specific handling
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageType {
    /// Profile Image (size)
    Profile(u32),
    /// Content Image with optional size hint
    Content(Option<PixelDimensions>),
}

#[cfg(test)]
mod tests {
    use super::{get_cached_request_state, TextureRequestKey, TextureRequestVariant};
    use crate::PixelDimensions;
    use hashbrown::HashMap;

    #[test]
    fn cached_request_state_hits_matching_url_and_variant() {
        let mut cache = HashMap::new();
        cache.insert(
            TextureRequestKey::from_variant(
                "https://example.com/image.png",
                TextureRequestVariant::Hint(PixelDimensions { x: 640, y: 480 }),
            ),
            42usize,
        );

        let state = get_cached_request_state(
            &cache,
            "https://example.com/image.png",
            TextureRequestVariant::Hint(PixelDimensions { x: 640, y: 480 }),
        );

        assert_eq!(state.copied(), Some(42));
    }

    #[test]
    fn cached_request_state_misses_when_variant_differs() {
        let mut cache = HashMap::new();
        cache.insert(
            TextureRequestKey::from_variant(
                "https://example.com/image.png",
                TextureRequestVariant::Full,
            ),
            1usize,
        );

        let miss = get_cached_request_state(
            &cache,
            "https://example.com/image.png",
            TextureRequestVariant::Hint(PixelDimensions { x: 800, y: 600 }),
        );

        assert!(miss.is_none());
    }

    #[test]
    fn cached_request_state_misses_when_url_differs() {
        let mut cache = HashMap::new();
        cache.insert(
            TextureRequestKey::from_variant(
                "https://example.com/image-a.png",
                TextureRequestVariant::Full,
            ),
            7usize,
        );

        let miss = get_cached_request_state(
            &cache,
            "https://example.com/image-b.png",
            TextureRequestVariant::Full,
        );

        assert!(miss.is_none());
    }
}
