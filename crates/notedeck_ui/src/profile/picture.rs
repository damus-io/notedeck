use egui::{vec2, InnerResponse, Sense, Stroke, TextureHandle};
use enostr::Pubkey;

use notedeck::get_render_state;
use notedeck::media::gif::ensure_latest_texture;
use notedeck::media::images::{fetch_no_pfp_promise, ImageType};
use notedeck::media::AnimationMode;
use notedeck::MediaAction;
use notedeck::{show_one_error_message, supported_mime_hosted_at_url, Accounts, Images, IsFollowing};

pub struct ProfilePic<'cache, 'url> {
    cache: &'cache mut Images,
    url: &'url str,
    size: f32,
    sense: Sense,
    border: Option<Stroke>,
    animation_mode: AnimationMode,
    pub action: Option<MediaAction>,
    pubkey: Option<&'url Pubkey>,
    accounts: Option<&'url Accounts>,
}

impl egui::Widget for &mut ProfilePic<'_, '_> {
    #[profiling::function]
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let inner = render_pfp(
            ui,
            self.cache,
            self.url,
            self.size,
            self.border,
            self.sense,
            self.animation_mode,
        );

        self.action = inner.inner;

        let should_show_badge = if let (Some(pubkey), Some(accounts)) = (self.pubkey, self.accounts) {
            let selected = accounts.get_selected_account();
            selected.key.pubkey == *pubkey || selected.is_following(pubkey.bytes()) == IsFollowing::Yes
        } else {
            false
        };

        if should_show_badge {
            let rect = inner.response.rect;
            let badge_size = (self.size * 0.4).max(12.0);
            let offset = badge_size * 0.25;
            let badge_pos = rect.right_top() + egui::vec2(-offset, offset);

            ui.painter().circle_filled(
                badge_pos,
                badge_size / 2.0,
                egui::Color32::from_rgb(139, 92, 246),
            );

            ui.painter().text(
                badge_pos,
                egui::Align2::CENTER_CENTER,
                "✓",
                egui::FontId::proportional(badge_size * 0.7),
                egui::Color32::WHITE,
            );
        }

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
            animation_mode: AnimationMode::Reactive,
            border: None,
            action: None,
            pubkey: None,
            accounts: None,
        }
    }

    pub fn with_follow_check(mut self, pubkey: &'url Pubkey, accounts: &'url Accounts) -> Self {
        self.pubkey = Some(pubkey);
        self.accounts = Some(accounts);
        self
    }

    pub fn sense(mut self, sense: Sense) -> Self {
        self.sense = sense;
        self
    }

    pub fn animation_mode(mut self, mode: AnimationMode) -> Self {
        self.animation_mode = mode;
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
    animation_mode: AnimationMode,
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
            profiling::scope!("Render pending");
            egui::InnerResponse::new(None, paint_circle(ui, ui_size, border, sense))
        }
        notedeck::TextureState::Error(e) => {
            profiling::scope!("Render error");
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
            profiling::scope!("Render loaded");
            let texture_handle =
                ensure_latest_texture(ui, url, cur_state.gifs, textured_image, animation_mode);

            egui::InnerResponse::new(None, pfp_image(ui, &texture_handle, ui_size, border, sense))
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
