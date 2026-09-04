use bitflags::bitflags;
use egui::{
    vec2, Button, Color32, Context, CornerRadius, FontId, Image, InnerResponse, Response,
    TextureHandle, Vec2,
};
use notedeck::media::latest::ObfuscatedTexture;
use notedeck::{
    fonts::get_font_size, show_one_error_message, tr, Images, Localization, MediaAction,
    MediaCacheType, NotedeckTextStyle, RenderableMedia,
};
use notedeck::{MediaJobSender, PointDimensions};

use crate::NoteOptions;
use notedeck::media::images::ImageType;
use notedeck::media::{AnimationMode, MediaRenderState};
use notedeck::media::{MediaInfo, ViewMediaInfo};

use crate::{app_images, AnimationHelper, PulseAlpha};

pub enum MediaViewAction {
    /// Used to handle escape presses when the media viewer is open
    EscapePressed,
}

#[allow(clippy::too_many_arguments)]
#[profiling::function]
pub fn image_carousel(
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    jobs: &MediaJobSender,
    medias: &[RenderableMedia],
    carousel_id: egui::Id,
    i18n: &mut Localization,
    note_options: NoteOptions,
) -> Option<MediaAction> {
    // let's make sure everything is within our area

    let size = {
        let height = 360.0;
        let width = ui.available_width();
        egui::vec2(width, height)
    };

    let mut action = None;

    //let has_touch_screen = ui.ctx().input(|i| i.has_touch_screen());
    ui.add_sized(size, |ui: &mut egui::Ui| {
        egui::ScrollArea::horizontal()
            .drag_to_scroll(false)
            .id_salt(carousel_id)
            .show(ui, |ui| {
                let response = ui
                    .horizontal(|ui| {
                        let spacing = ui.spacing_mut();
                        spacing.item_spacing.x = 8.0;

                        let mut media_infos: Vec<MediaInfo> = Vec::with_capacity(medias.len());
                        let mut media_action: Option<(usize, MediaUIAction)> = None;

                        for (i, media) in medias.iter().enumerate() {
                            let media_response = render_media(
                                ui,
                                img_cache,
                                jobs,
                                media,
                                note_options.contains(NoteOptions::TrustMedia)
                                    || img_cache.user_trusts_img(&media.url, media.media_type),
                                i18n,
                                size,
                                if note_options.contains(NoteOptions::NoAnimations) {
                                    Some(AnimationMode::NoAnimation)
                                } else {
                                    None
                                },
                                if note_options.contains(NoteOptions::Wide) {
                                    ScaledTextureFlags::SCALE_TO_WIDTH
                                } else {
                                    ScaledTextureFlags::empty()
                                },
                            );

                            if let Some(media_ui_action) = media_response.inner {
                                if media_ui_action.requires_app_action() {
                                    media_action = Some((i, media_ui_action));
                                } else {
                                    let image_type = ImageType::Content(Some(
                                        PointDimensions::from_vec(size).to_pixels(ui),
                                    ));
                                    process_local_media_action(
                                        media_ui_action,
                                        &media.url,
                                        media.media_type,
                                        image_type,
                                        img_cache,
                                        jobs,
                                        ui.ctx(),
                                    );
                                }
                            }

                            let rect = media_response.response.rect;
                            media_infos.push(MediaInfo {
                                url: media.url.clone(),
                                original_position: rect,
                            })
                        }

                        if let Some((i, media_action)) = media_action {
                            action = media_action.into_media_action(
                                medias,
                                media_infos,
                                i,
                                img_cache,
                                ImageType::Content(Some(
                                    PointDimensions::from_vec(size).to_pixels(ui),
                                )),
                            );
                        }
                    })
                    .response;
                ui.add_space(notedeck::tokens::SPACING_SM);
                response
            })
            .inner
    });

    action
}

#[allow(clippy::too_many_arguments)]
pub fn render_media(
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    jobs: &MediaJobSender,
    media: &RenderableMedia,
    trusted_media: bool,
    i18n: &mut Localization,
    size: Vec2,
    animation_mode: Option<AnimationMode>,
    scale_flags: ScaledTextureFlags,
) -> InnerResponse<Option<MediaUIAction>> {
    let RenderableMedia {
        url,
        media_type,
        obfuscation_type: blur_type,
    } = media;

    let animation_mode = animation_mode.unwrap_or_else(|| {
        // if animations aren't disabled, we cap it at 24fps for gifs in carousels
        let fps = match media_type {
            MediaCacheType::Gif => Some(24.0),
            MediaCacheType::Image => None,
        };
        AnimationMode::Continuous { fps }
    });
    let points = PointDimensions::from_vec(size);
    let media_state = if trusted_media {
        img_cache.trusted_texture_loader().latest(
            jobs,
            ui.ctx(),
            url,
            *media_type,
            ImageType::Content(Some(points.to_pixels(ui))),
            animation_mode,
            blur_type,
            points,
        )
    } else {
        img_cache
            .untrusted_texture_loader()
            .latest(jobs, ui.ctx(), url, blur_type, points)
    };

    render_media_internal(ui, media_state, url, size, i18n, scale_flags)
}

pub enum MediaUIAction {
    Unblur,
    Error,
    DoneLoading,
    Clicked,
}

impl MediaUIAction {
    fn requires_app_action(&self) -> bool {
        matches!(self, Self::Clicked)
    }

    pub fn into_media_action(
        self,
        medias: &[RenderableMedia],
        responses: Vec<MediaInfo>,
        selected: usize,
        img_cache: &Images,
        img_type: ImageType,
    ) -> Option<MediaAction> {
        match self {
            // We've clicked on some media, let's package up
            // all of the rendered media responses, and send
            // them to the ViewMedias action so that our fullscreen
            // media viewer can smoothly transition from them
            MediaUIAction::Clicked => Some(MediaAction::ViewMedias(ViewMediaInfo {
                clicked_index: selected,
                medias: responses,
            })),

            MediaUIAction::Unblur => {
                let url = &medias[selected].url;
                let cache = img_cache.get_cache(medias[selected].media_type);
                let cache_type = cache.cache_type;
                Some(MediaAction::FetchImage {
                    url: url.to_owned(),
                    cache_type,
                })
            }

            MediaUIAction::Error => {
                if !matches!(img_type, ImageType::Profile(_)) {
                    return None;
                };

                let cache = img_cache.get_cache(medias[selected].media_type);
                let cache_type = cache.cache_type;
                Some(MediaAction::FetchImage {
                    url: medias[selected].url.to_owned(),
                    cache_type,
                })
            }
            MediaUIAction::DoneLoading => Some(MediaAction::DoneLoading {
                url: medias[selected].url.to_owned(),
                cache_type: img_cache.get_cache(medias[selected].media_type).cache_type,
            }),
        }
    }
}

fn process_local_media_action(
    action: MediaUIAction,
    url: &str,
    media_type: MediaCacheType,
    image_type: ImageType,
    img_cache: &mut Images,
    jobs: &MediaJobSender,
    ctx: &Context,
) {
    match action {
        MediaUIAction::DoneLoading => img_cache.textures.blurred.finished_transitioning(url),
        MediaUIAction::Unblur => match media_type {
            MediaCacheType::Image => img_cache
                .textures
                .static_image
                .request(jobs, ctx, url, image_type),
            MediaCacheType::Gif => img_cache
                .textures
                .animated
                .request(jobs, ctx, url, image_type),
        },
        MediaUIAction::Error | MediaUIAction::Clicked => {}
    }
}

fn copy_link(i18n: &mut Localization, url: &str, img_resp: &Response) {
    crate::context_menu::context_menu(img_resp, |ui| {
        if ui
            .button(tr!(
                i18n,
                "Copy Link",
                "Button to copy media link to clipboard"
            ))
            .clicked()
        {
            ui.ctx().copy_text(url.to_owned());
            ui.close_menu();
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn render_media_internal(
    ui: &mut egui::Ui,
    render_state: MediaRenderState,
    url: &str,
    size: egui::Vec2,
    i18n: &mut Localization,
    scale_flags: ScaledTextureFlags,
) -> egui::InnerResponse<Option<MediaUIAction>> {
    match render_state {
        MediaRenderState::ActualImage(image) => {
            let resp = render_success_media(ui, url, image, size, i18n, scale_flags);
            if resp.clicked() {
                egui::InnerResponse::new(Some(MediaUIAction::Clicked), resp)
            } else {
                egui::InnerResponse::new(None, resp)
            }
        }
        MediaRenderState::Transitioning {
            image: img_tex,
            obfuscation,
        } => match obfuscation {
            ObfuscatedTexture::Blur(blur_tex) => {
                let resp = render_blur_transition(ui, url, size, blur_tex, img_tex, scale_flags);
                if resp.inner {
                    egui::InnerResponse::new(Some(MediaUIAction::DoneLoading), resp.response)
                } else {
                    egui::InnerResponse::new(None, resp.response)
                }
            }
            ObfuscatedTexture::Default => {
                let scaled = ScaledTexture::new(img_tex, size, scale_flags);
                let resp = ui.add(scaled.get_image());
                egui::InnerResponse::new(Some(MediaUIAction::DoneLoading), resp)
            }
        },
        MediaRenderState::Error(e) => {
            let response = ui.allocate_response(size, egui::Sense::hover());
            show_one_error_message(ui, &format!("Could not render media {url}: {e}"));
            egui::InnerResponse::new(Some(MediaUIAction::Error), response)
        }
        MediaRenderState::Shimmering(obfuscated_texture) => match obfuscated_texture {
            ObfuscatedTexture::Blur(texture_handle) => egui::InnerResponse::new(
                None,
                shimmer_blurhash(texture_handle, ui, url, size, scale_flags),
            ),
            ObfuscatedTexture::Default => {
                let shimmer = true;
                egui::InnerResponse::new(
                    None,
                    render_default_blur_bg(
                        ui,
                        size,
                        url,
                        shimmer,
                        scale_flags.contains(ScaledTextureFlags::SCALE_TO_WIDTH),
                    ),
                )
            }
        },
        MediaRenderState::Obfuscated(obfuscated_texture) => {
            let resp = match obfuscated_texture {
                ObfuscatedTexture::Blur(texture_handle) => {
                    let scaled = ScaledTexture::blurred(texture_handle, size, scale_flags);

                    let resp = ui.add(scaled.get_image());
                    render_blur_text(ui, i18n, url, resp.rect)
                }
                ObfuscatedTexture::Default => render_default_blur(
                    ui,
                    i18n,
                    size,
                    url,
                    scale_flags.contains(ScaledTextureFlags::SCALE_TO_WIDTH),
                ),
            };

            let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                egui::InnerResponse::new(Some(MediaUIAction::Unblur), resp)
            } else {
                egui::InnerResponse::new(None, resp)
            }
        }
    }
}

fn render_blur_text(
    ui: &mut egui::Ui,
    i18n: &mut Localization,
    url: &str,
    render_rect: egui::Rect,
) -> egui::Response {
    let helper = AnimationHelper::new_from_rect(ui, ("show_media", url), render_rect);

    let painter = ui.painter_at(helper.get_animation_rect());

    let text_style = NotedeckTextStyle::Button;

    let icon_size = helper.scale_1d_pos(30.0);
    let animation_fontid = FontId::new(
        helper.scale_1d_pos(get_font_size(ui.ctx(), &text_style)),
        text_style.font_family(),
    );
    let info_galley = painter.layout(
        tr!(
            i18n,
            "Media from someone you don't follow",
            "Text shown on blurred media from unfollowed users"
        )
        .to_owned(),
        animation_fontid.clone(),
        ui.visuals().text_color(),
        render_rect.width() / 2.0,
    );

    let load_galley = painter.layout_no_wrap(
        tr!(i18n, "Tap to Load", "Button text to load blurred media"),
        animation_fontid,
        egui::Color32::BLACK,
        // ui.visuals().widgets.inactive.bg_fill,
    );

    let items_height = info_galley.rect.height() + load_galley.rect.height() + icon_size;

    let spacing = helper.scale_1d_pos(8.0);
    let icon_rect = {
        let mut center = helper.get_animation_rect().center();
        center.y -= (items_height / 2.0) + (spacing * 3.0) - (icon_size / 2.0);

        egui::Rect::from_center_size(center, egui::vec2(icon_size, icon_size))
    };

    (if ui.visuals().dark_mode {
        app_images::eye_slash_dark_image()
    } else {
        app_images::eye_slash_light_image()
    })
    .max_width(icon_size)
    .paint_at(ui, icon_rect);

    let info_galley_pos = {
        let mut pos = icon_rect.center();
        pos.x -= info_galley.rect.width() / 2.0;
        pos.y = icon_rect.bottom() + spacing;
        pos
    };

    let load_galley_pos = {
        let mut pos = icon_rect.center();
        pos.x -= load_galley.rect.width() / 2.0;
        pos.y = icon_rect.bottom() + info_galley.rect.height() + (4.0 * spacing);
        pos
    };

    let button_rect = egui::Rect::from_min_size(load_galley_pos, load_galley.size()).expand(8.0);

    let button_fill = egui::Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0x1F);

    painter.rect(
        button_rect,
        egui::CornerRadius::same(notedeck::tokens::RADIUS_MD as u8),
        button_fill,
        egui::Stroke::NONE,
        egui::StrokeKind::Middle,
    );

    painter.galley(info_galley_pos, info_galley, egui::Color32::WHITE);
    painter.galley(load_galley_pos, load_galley, egui::Color32::WHITE);

    helper.take_animation_response()
}

fn render_default_blur(
    ui: &mut egui::Ui,
    i18n: &mut Localization,
    size: egui::Vec2,
    url: &str,
    is_scaled: bool,
) -> egui::Response {
    let shimmer = false;
    let response = render_default_blur_bg(ui, size, url, shimmer, is_scaled);
    render_blur_text(ui, i18n, url, response.rect)
}

fn render_default_blur_bg(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    url: &str,
    shimmer: bool,
    is_scaled: bool,
) -> egui::Response {
    let size = if is_scaled {
        size
    } else {
        vec2(size.y, size.y)
    };

    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    let painter = ui.painter_at(rect);

    let mut color = crate::colors::MID_GRAY;
    if shimmer {
        let [r, g, b, _a] = color.to_srgba_unmultiplied();
        let cur_alpha = get_blur_current_alpha(ui, url);
        color = Color32::from_rgba_unmultiplied(r, g, b, cur_alpha)
    }

    painter.rect_filled(rect, CornerRadius::same(8), color);

    response
}

#[allow(clippy::too_many_arguments)]
fn render_success_media(
    ui: &mut egui::Ui,
    url: &str,
    tex: &TextureHandle,
    size: Vec2,
    i18n: &mut Localization,
    scale_flags: ScaledTextureFlags,
) -> Response {
    let scaled = ScaledTexture::new(tex, size, scale_flags);

    let img_resp = ui.add(Button::image(scaled.get_image()).frame(false));

    copy_link(i18n, url, &img_resp);

    img_resp
}

fn texture_to_image<'a>(tex: &TextureHandle, size: Vec2) -> egui::Image<'a> {
    Image::new(tex)
        .corner_radius(5.0)
        .fit_to_exact_size(size)
        .maintain_aspect_ratio(true)
}

static BLUR_SHIMMER_ID: fn(&str) -> egui::Id = |url| egui::Id::new(("blur_shimmer", url));

fn get_blur_current_alpha(ui: &mut egui::Ui, url: &str) -> u8 {
    let id = BLUR_SHIMMER_ID(url);

    let (alpha_min, alpha_max) = if ui.visuals().dark_mode {
        (150, 255)
    } else {
        (220, 255)
    };
    PulseAlpha::new(ui.ctx(), id, alpha_min, alpha_max)
        .with_speed(0.3)
        .start_max_alpha()
        .animate()
}

fn shimmer_blurhash(
    tex: &TextureHandle,
    ui: &mut egui::Ui,
    url: &str,
    size: Vec2,
    scale_flags: ScaledTextureFlags,
) -> egui::Response {
    let cur_alpha = get_blur_current_alpha(ui, url);

    let scaled = ScaledTexture::blurred(tex, size, scale_flags);
    let img = scaled.get_image();
    show_blurhash_with_alpha(ui, img, cur_alpha)
}

fn fade_color(alpha: u8) -> egui::Color32 {
    Color32::from_rgba_unmultiplied(255, 255, 255, alpha)
}

fn show_blurhash_with_alpha(ui: &mut egui::Ui, img: Image, alpha: u8) -> egui::Response {
    let cur_color = fade_color(alpha);
    let img = img.tint(cur_color);

    ui.add(img)
}

fn show_blurhash_with_alpha_at_size(
    ui: &mut egui::Ui,
    img: Image,
    alpha: u8,
    size: Vec2,
) -> egui::Response {
    let cur_color = fade_color(alpha);
    let img = img.tint(cur_color);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    img.paint_at(ui, rect);
    response
}

type FinishedTransition = bool;

// return true if transition is finished
fn render_blur_transition(
    ui: &mut egui::Ui,
    url: &str,
    size: Vec2,
    blur_texture: &TextureHandle,
    image_texture: &TextureHandle,
    scale_flags: ScaledTextureFlags,
) -> egui::InnerResponse<FinishedTransition> {
    let scaled_texture = ScaledTexture::new(image_texture, size, scale_flags);
    let scaled_blur_img = ScaledTexture::blurred(blur_texture, size, scale_flags);

    match get_blur_transition_state(ui.ctx(), url) {
        BlurTransitionState::StoppingShimmer { cur_alpha } => egui::InnerResponse::new(
            false,
            show_blurhash_with_alpha_at_size(
                ui,
                scaled_blur_img.get_image(),
                cur_alpha,
                scaled_texture.scaled_size,
            ),
        ),
        BlurTransitionState::FadingBlur => {
            render_blur_fade(ui, url, scaled_blur_img.get_image(), &scaled_texture)
        }
    }
}

struct ScaledTexture<'a> {
    tex: &'a TextureHandle,
    size: Vec2,
    pub scaled_size: Vec2,
}

bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ScaledTextureFlags: u8 {
        const SCALE_TO_WIDTH = 1u8;
        const RESPECT_MAX_DIMS = 2u8;
    }
}

impl<'a> ScaledTexture<'a> {
    pub fn new(tex: &'a TextureHandle, max_size: Vec2, flags: ScaledTextureFlags) -> Self {
        let tex_size = tex.size_vec2();

        if flags.contains(ScaledTextureFlags::RESPECT_MAX_DIMS) {
            return Self::respecting_max(tex, max_size);
        }

        let scaled_size = if !flags.contains(ScaledTextureFlags::SCALE_TO_WIDTH) {
            if tex_size.y > max_size.y {
                let scale = max_size.y / tex_size.y;
                tex_size * scale
            } else {
                tex_size
            }
        } else if tex_size.x != max_size.x {
            let scale = max_size.x / tex_size.x;
            tex_size * scale
        } else {
            tex_size
        };

        Self {
            tex,
            size: max_size,
            scaled_size,
        }
    }

    /// Fits a blurhash placeholder to `max_size`.
    ///
    /// The same geometry as [`Self::new`], except that the texture is scaled
    /// *up* as readily as down. Placeholders are decoded at a fixed small size
    /// (see `notedeck::media::blur`), so their texel count says nothing about
    /// how large they should be drawn — only their aspect ratio does, and only
    /// so that the placeholder is the shape of the media it stands in for.
    pub fn blurred(tex: &'a TextureHandle, max_size: Vec2, flags: ScaledTextureFlags) -> Self {
        // respecting_max already scales in both directions.
        if flags.contains(ScaledTextureFlags::RESPECT_MAX_DIMS) {
            return Self::respecting_max(tex, max_size);
        }

        let tex_size = tex.size_vec2();
        let scale = if flags.contains(ScaledTextureFlags::SCALE_TO_WIDTH) {
            max_size.x / tex_size.x
        } else {
            max_size.y / tex_size.y
        };

        Self {
            tex,
            size: max_size,
            scaled_size: tex_size * scale,
        }
    }

    pub fn respecting_max(tex: &'a TextureHandle, max_size: Vec2) -> Self {
        let tex_size = tex.size_vec2();

        let s = (max_size.x / tex_size.x).min(max_size.y / tex_size.y);
        let scaled_size = tex_size * s;

        Self {
            tex,
            size: max_size,
            scaled_size,
        }
    }

    pub fn get_image(&self) -> Image<'_> {
        texture_to_image(self.tex, self.size).fit_to_exact_size(self.scaled_size)
    }
}

fn render_blur_fade(
    ui: &mut egui::Ui,
    url: &str,
    blur_img: Image,
    image_texture: &ScaledTexture,
) -> egui::InnerResponse<FinishedTransition> {
    let blur_fade_id = ui.id().with(("blur_fade", url));

    let cur_alpha = {
        PulseAlpha::new(ui.ctx(), blur_fade_id, 0, 255)
            .start_max_alpha()
            .with_speed(0.3)
            .animate()
    };

    let img = image_texture.get_image();

    let blur_img = blur_img.tint(fade_color(cur_alpha));

    let alloc_size = image_texture.scaled_size;

    let (rect, resp) = ui.allocate_exact_size(alloc_size, egui::Sense::hover());

    img.paint_at(ui, rect);
    blur_img.paint_at(ui, rect);

    egui::InnerResponse::new(cur_alpha == 0, resp)
}

fn get_blur_transition_state(ctx: &Context, url: &str) -> BlurTransitionState {
    let shimmer_id = BLUR_SHIMMER_ID(url);

    let max_alpha = 255.0;
    let cur_shimmer_alpha = ctx.animate_value_with_time(shimmer_id, max_alpha, 0.3);
    if cur_shimmer_alpha == max_alpha {
        BlurTransitionState::FadingBlur
    } else {
        let cur_alpha = (cur_shimmer_alpha).clamp(0.0, max_alpha) as u8;
        BlurTransitionState::StoppingShimmer { cur_alpha }
    }
}

enum BlurTransitionState {
    StoppingShimmer { cur_alpha: u8 },
    FadingBlur,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real 4x3-component blurhash — the `blurhash` crate's octocat fixture.
    const OCTOCAT: &str = "LNAdAqj[00aymkj[TKay9}ay-Sj[";

    /// A column-sized media slot: `image_carousel` allocates `available_width`
    /// by a fixed 360 points.
    const SLOT: Vec2 = vec2(400.0, 360.0);

    fn blur_texture(ctx: &egui::Context, w: u32, h: u32) -> TextureHandle {
        let bytes = blurhash::decode(OCTOCAT, w, h, 1.0).expect("decodes");
        let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &bytes);
        notedeck::media::load_texture_checked(
            ctx,
            format!("blur-{w}x{h}"),
            img,
            egui::TextureOptions::LINEAR,
        )
    }

    #[test]
    fn a_capped_blur_fills_the_same_box_a_display_sized_one_did() {
        let ctx = egui::Context::default();

        // What the blur cache used to hand over for this slot on a 2x display,
        // and what it hands over now.
        let display_sized = blur_texture(&ctx, 1280, 720);
        let capped = blur_texture(&ctx, 64, 36);

        for flags in [
            ScaledTextureFlags::empty(),
            ScaledTextureFlags::SCALE_TO_WIDTH,
        ] {
            assert_eq!(
                ScaledTexture::blurred(&capped, SLOT, flags).scaled_size,
                ScaledTexture::new(&display_sized, SLOT, flags).scaled_size,
                "{flags:?} placeholder changed size",
            );
        }
    }

    #[test]
    fn the_plain_constructor_would_have_drawn_a_capped_blur_at_its_texel_size() {
        // Why `blurred` exists: `new` only ever scales down, so a placeholder
        // decoded below the slot size would be drawn as a 64x36 point postage
        // stamp instead of filling the slot.
        let ctx = egui::Context::default();
        let capped = blur_texture(&ctx, 64, 36);

        assert_eq!(
            ScaledTexture::new(&capped, SLOT, ScaledTextureFlags::empty()).scaled_size,
            vec2(64.0, 36.0),
        );
    }
}
