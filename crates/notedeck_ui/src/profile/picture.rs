use crate::gif::{handle_repaint, retrieve_latest_texture};
use crate::images::{ImageType, fetch_no_pfp_promise, get_render_state};
use egui::{InnerResponse, Sense, Stroke, TextureHandle, vec2};

use notedeck::note::MediaAction;
use notedeck::{Images, show_one_error_message, supported_mime_hosted_at_url};

pub struct ProfilePic<'cache, 'url> {
    cache: &'cache mut Images,
    url: &'url str,
    size: f32,
    sense: Sense,
    border: Option<Stroke>,
    pub action: Option<MediaAction>,
}

impl egui::Widget for &mut ProfilePic<'_, '_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let inner = render_pfp(ui, self.cache, self.url, self.size, self.border, self.sense);

        self.action = inner.inner;

        inner.response
    }
}

impl<'cache, 'url> ProfilePic<'cache, 'url> {
    pub fn new(cache: &'cache mut Images, url: &'url str) -> Self {
        let size = Self::default_size() as f32;
        let sense = Sense::hover();

        ProfilePic {
            cache,
            sense,
            url,
            size,
            border: None,
            action: None,
        }
    }

    pub fn sense(mut self, sense: Sense) -> Self {
        self.sense = sense;
        self
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

    pub fn from_profile_or_default(
        cache: &'cache mut Images,
        profile: Option<&nostrdb::ProfileRecord<'url>>,
    ) -> Self {
        let url = profile
            .map(|p| p.record())
            .and_then(|p| p.profile())
            .and_then(|p| p.picture())
            .unwrap_or(notedeck::profile::no_pfp_url());

        ProfilePic::new(cache, url)
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
    sense: Sense,
) -> InnerResponse<Option<MediaAction>> {
    // We will want to downsample these so it's not blurry on hi res displays
    let img_size = 128u32;

    let cache_type = supported_mime_hosted_at_url(&mut img_cache.urls, url)
        .unwrap_or(notedeck::MediaCacheType::Image);

    let cur_state = get_render_state(
        ui.ctx(),
        img_cache,
        cache_type,
        url,
        ImageType::Profile(img_size),
    );

    match cur_state.texture_state {
        notedeck::TextureState::Pending => {
            egui::InnerResponse::new(None, paint_circle(ui, ui_size, border, sense))
        }
        notedeck::TextureState::Error(e) => {
            let r = paint_circle(ui, ui_size, border, sense);
            show_one_error_message(ui, &format!("Failed to fetch profile at url {url}: {e}"));
            egui::InnerResponse::new(
                Some(MediaAction::FetchImage {
                    url: url.to_owned(),
                    cache_type,
                    no_pfp_promise: fetch_no_pfp_promise(ui.ctx(), img_cache.get_cache(cache_type)),
                }),
                r,
            )
        }
        notedeck::TextureState::Loaded(textured_image) => {
            let texture_handle = handle_repaint(
                ui,
                retrieve_latest_texture(url, cur_state.gifs, textured_image),
            );

            egui::InnerResponse::new(None, pfp_image(ui, texture_handle, ui_size, border, sense))
        }
    }
}

#[profiling::function]
fn pfp_image(
    ui: &mut egui::Ui,
    img: &TextureHandle,
    size: f32,
    border: Option<Stroke>,
    sense: Sense,
) -> egui::Response {
    let (rect, response) = ui.allocate_at_least(vec2(size, size), sense);
    if let Some(stroke) = border {
        draw_bg_border(ui, rect.center(), size, stroke);
    }
    ui.put(rect, egui::Image::new(img).max_width(size));

    response
}

fn paint_circle(
    ui: &mut egui::Ui,
    size: f32,
    border: Option<Stroke>,
    sense: Sense,
) -> egui::Response {
    let (rect, response) = ui.allocate_at_least(vec2(size, size), sense);

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
