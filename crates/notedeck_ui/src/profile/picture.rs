use crate::gif::{handle_repaint, retrieve_latest_texture};
use crate::images::{render_images, ImageType};
use egui::{vec2, Sense, Stroke, TextureHandle};

use notedeck::{supported_mime_hosted_at_url, Images};

pub struct ProfilePic<'cache, 'url> {
    cache: &'cache mut Images,
    url: &'url str,
    size: f32,
    border: Option<Stroke>,
}

impl egui::Widget for ProfilePic<'_, '_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        render_pfp(ui, self.cache, self.url, self.size, self.border)
    }
}

impl<'cache, 'url> ProfilePic<'cache, 'url> {
    pub fn new(cache: &'cache mut Images, url: &'url str) -> Self {
        let size = Self::default_size() as f32;
        ProfilePic {
            cache,
            url,
            size,
            border: None,
        }
    }

    pub fn border_stroke(ui: &egui::Ui) -> Stroke {
        Stroke::new(4.0, ui.visuals().panel_fill)
    }

    pub fn from_profile(
        cache: &'cache mut Images,
        profile: &nostrdb::ProfileRecord<'url>,
    ) -> Option<Self> {
        profile
            .record()
            .profile()
            .and_then(|p| p.picture())
            .map(|url| ProfilePic::new(cache, url))
    }

    #[inline]
    pub fn default_size() -> i8 {
        38
    }

    #[inline]
    pub fn medium_size() -> i8 {
        32
    }

    #[inline]
    pub fn small_size() -> i8 {
        24
    }

    #[inline]
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    #[inline]
    pub fn border(mut self, stroke: Stroke) -> Self {
        self.border = Some(stroke);
        self
    }
}

#[profiling::function]
fn render_pfp(
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    url: &str,
    ui_size: f32,
    border: Option<Stroke>,
) -> egui::Response {
    // We will want to downsample these so it's not blurry on hi res displays
    let img_size = 128u32;

    let cache_type = supported_mime_hosted_at_url(&mut img_cache.urls, url)
        .unwrap_or(notedeck::MediaCacheType::Image);

    render_images(
        ui,
        img_cache,
        url,
        ImageType::Profile(img_size),
        cache_type,
        |ui| {
            paint_circle(ui, ui_size, border);
        },
        |ui, _| {
            paint_circle(ui, ui_size, border);
        },
        |ui, url, renderable_media, gifs| {
            let texture_handle =
                handle_repaint(ui, retrieve_latest_texture(url, gifs, renderable_media));
            pfp_image(ui, texture_handle, ui_size, border);
        },
    )
}

#[profiling::function]
fn pfp_image(
    ui: &mut egui::Ui,
    img: &TextureHandle,
    size: f32,
    border: Option<Stroke>,
) -> egui::Response {
    let (rect, response) = ui.allocate_at_least(vec2(size, size), Sense::hover());
    if let Some(stroke) = border {
        draw_bg_border(ui, rect.center(), size, stroke);
    }
    ui.put(rect, egui::Image::new(img).max_width(size));

    response
}

fn paint_circle(ui: &mut egui::Ui, size: f32, border: Option<Stroke>) -> egui::Response {
    let (rect, response) = ui.allocate_at_least(vec2(size, size), Sense::hover());

    if let Some(stroke) = border {
        draw_bg_border(ui, rect.center(), size, stroke);
    }

    ui.painter()
        .circle_filled(rect.center(), size / 2.0, ui.visuals().weak_text_color());

    response
}

fn draw_bg_border(ui: &mut egui::Ui, center: egui::Pos2, size: f32, stroke: Stroke) {
    let border_size = size + (stroke.width * 2.0);
    ui.painter()
        .circle_filled(center, border_size / 2.0, stroke.color);
}
