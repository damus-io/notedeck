use std::collections::HashMap;

use nostrdb::Note;

use crate::jobs::{Job, JobError, JobParamsOwned};

#[derive(Clone)]
pub struct Blur<'a> {
    pub blurhash: &'a str,
    pub dimensions: Option<PixelDimensions>, // width and height in pixels
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

impl Blur<'_> {
    pub fn scaled_pixel_dimensions(
        &self,
        ui: &egui::Ui,
        available_points: PointDimensions,
    ) -> PixelDimensions {
        let max_pixels = available_points.to_pixels(ui);

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

pub fn imeta_blurhashes<'a>(note: &'a Note) -> HashMap<&'a str, Blur<'a>> {
    let mut blurs = HashMap::new();

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

        blurs.insert(url, blur);
    }

    blurs
}

fn find_blur(tag_iter: nostrdb::TagIter) -> Option<(&str, Blur)> {
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
        url,
        Blur {
            blurhash,
            dimensions,
        },
    ))
}

#[derive(Clone)]
pub enum ObfuscationType<'a> {
    Blurhash(Blur<'a>),
    Default,
}

pub(crate) fn compute_blurhash(
    params: Option<JobParamsOwned>,
    dims: PixelDimensions,
) -> Result<Job, JobError> {
    #[allow(irrefutable_let_patterns)]
    let Some(JobParamsOwned::Blurhash(params)) = params
    else {
        return Err(JobError::InvalidParameters);
    };

    let maybe_handle = match generate_blurhash_texturehandle(
        &params.ctx,
        &params.blurhash,
        &params.url,
        dims.x,
        dims.y,
    ) {
        Ok(tex) => Some(tex),
        Err(e) => {
            tracing::error!("failed to render blurhash: {e}");
            None
        }
    };

    Ok(Job::Blurhash(maybe_handle))
}

fn generate_blurhash_texturehandle(
    ctx: &egui::Context,
    blurhash: &str,
    url: &str,
    width: u32,
    height: u32,
) -> notedeck::Result<egui::TextureHandle> {
    let bytes = blurhash::decode(blurhash, width, height, 1.0)
        .map_err(|e| notedeck::Error::Generic(e.to_string()))?;

    let img = egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &bytes);
    Ok(ctx.load_texture(url, img, Default::default()))
}
