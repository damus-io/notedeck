use std::path::Path;

use egui::{
    vec2, Button, Color32, Context, CornerRadius, FontId, Image, Response, TextureHandle, Vec2,
};
use notedeck::{
    compute_blurhash, fonts::get_font_size, show_one_error_message, tr, BlurhashParams,
    GifStateMap, Images, Job, JobId, JobParams, JobPool, JobState, JobsCache, Localization,
    MediaAction, MediaCacheType, NotedeckTextStyle, ObfuscationType, PointDimensions,
    RenderableMedia, TexturedImage, TexturesCache,
};

use crate::NoteOptions;
use notedeck::media::gif::ensure_latest_texture;
use notedeck::media::images::{fetch_no_pfp_promise, ImageType};
use notedeck::media::{MediaInfo, ViewMediaInfo};

use crate::{app_images, AnimationHelper, PulseAlpha};

pub enum MediaViewAction {
    /// Used to handle escape presses when the media viewer is open
    EscapePressed,
}

#[allow(clippy::too_many_arguments)]
pub fn image_carousel(
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    job_pool: &mut JobPool,
    jobs: &mut JobsCache,
    medias: &[RenderableMedia],
    carousel_id: egui::Id,
    trusted_media: bool,
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
                            let RenderableMedia {
                                url,
                                media_type,
                                obfuscation_type: blur_type,
                            } = media;

                            let cache = match media_type {
                                MediaCacheType::Image => &mut img_cache.static_imgs,
                                MediaCacheType::Gif => &mut img_cache.gifs,
                            };
                            let media_state = get_content_media_render_state(
                                ui,
                                job_pool,
                                jobs,
                                trusted_media,
                                size,
                                &mut cache.textures_cache,
                                url,
                                *media_type,
                                &cache.cache_dir,
                                blur_type,
                            );

                            let media_response = render_media(
                                ui,
                                &mut img_cache.gif_states,
                                media_state,
                                url,
                                size,
                                i18n,
                                note_options.contains(NoteOptions::Wide),
                            );

                            if let Some(action) = media_response.inner {
                                media_action = Some((i, action))
                            }

                            let rect = media_response.response.rect;
                            media_infos.push(MediaInfo {
                                url: url.clone(),
                                original_position: rect,
                            })
                        }

                        if let Some((i, media_action)) = media_action {
                            action = media_action.into_media_action(
                                ui.ctx(),
                                medias,
                                media_infos,
                                i,
                                img_cache,
                                ImageType::Content(Some((size.x as u32, size.y as u32))),
                            );
                        }
                    })
                    .response;
                ui.add_space(8.0);
                response
            })
            .inner
    });

    action
}

enum MediaUIAction {
    Unblur,
    Error,
    DoneLoading,
    Clicked,
}

impl MediaUIAction {
    pub fn into_media_action(
        self,
        ctx: &egui::Context,
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
                let no_pfp_promise = notedeck::media::images::fetch_img(
                    &cache.cache_dir,
                    ctx,
                    url,
                    img_type,
                    cache_type,
                );
                Some(MediaAction::FetchImage {
                    url: url.to_owned(),
                    cache_type,
                    no_pfp_promise,
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
                    no_pfp_promise: fetch_no_pfp_promise(ctx, cache),
                })
            }
            MediaUIAction::DoneLoading => Some(MediaAction::DoneLoading {
                url: medias[selected].url.to_owned(),
                cache_type: img_cache.get_cache(medias[selected].media_type).cache_type,
            }),
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn get_content_media_render_state<'a>(
    ui: &mut egui::Ui,
    job_pool: &'a mut JobPool,
    jobs: &'a mut JobsCache,
    media_trusted: bool,
    size: Vec2,
    cache: &'a mut TexturesCache,
    url: &'a str,
    cache_type: MediaCacheType,
    cache_dir: &Path,
    obfuscation_type: &'a ObfuscationType,
) -> MediaRenderState<'a> {
    let render_type = if media_trusted {
        cache.handle_and_get_or_insert_loadable(url, || {
            notedeck::media::images::fetch_img(
                cache_dir,
                ui.ctx(),
                url,
                ImageType::Content(None),
                cache_type,
            )
        })
    } else if let Some(render_type) = cache.get_and_handle(url) {
        render_type
    } else {
        return MediaRenderState::Obfuscated(get_obfuscated(
            ui,
            url,
            obfuscation_type,
            job_pool,
            jobs,
            size,
        ));
    };

    match render_type {
        notedeck::LoadableTextureState::Pending => MediaRenderState::Shimmering(get_obfuscated(
            ui,
            url,
            obfuscation_type,
            job_pool,
            jobs,
            size,
        )),
        notedeck::LoadableTextureState::Error(e) => MediaRenderState::Error(e),
        notedeck::LoadableTextureState::Loading { actual_image_tex } => {
            let obfuscation = get_obfuscated(ui, url, obfuscation_type, job_pool, jobs, size);
            MediaRenderState::Transitioning {
                image: actual_image_tex,
                obfuscation,
            }
        }
        notedeck::LoadableTextureState::Loaded(textured_image) => {
            MediaRenderState::ActualImage(textured_image)
        }
    }
}

fn get_obfuscated<'a>(
    ui: &mut egui::Ui,
    url: &str,
    obfuscation_type: &'a ObfuscationType,
    job_pool: &'a mut JobPool,
    jobs: &'a mut JobsCache,
    size: Vec2,
) -> ObfuscatedTexture<'a> {
    let ObfuscationType::Blurhash(renderable_blur) = obfuscation_type else {
        return ObfuscatedTexture::Default;
    };

    let params = BlurhashParams {
        blurhash: &renderable_blur.blurhash,
        url,
        ctx: ui.ctx(),
    };

    let available_points = PointDimensions {
        x: size.x,
        y: size.y,
    };

    let pixel_sizes = renderable_blur.scaled_pixel_dimensions(ui, available_points);

    let job_state = jobs.get_or_insert_with(
        job_pool,
        &JobId::Blurhash(url),
        Some(JobParams::Blurhash(params)),
        move |params| compute_blurhash(params, pixel_sizes),
    );

    let JobState::Completed(m_blur_job) = job_state else {
        return ObfuscatedTexture::Default;
    };

    #[allow(irrefutable_let_patterns)]
    let Job::Blurhash(m_texture_handle) = m_blur_job
    else {
        tracing::error!("Did not get the correct job type: {:?}", m_blur_job);
        return ObfuscatedTexture::Default;
    };

    let Some(texture_handle) = m_texture_handle else {
        return ObfuscatedTexture::Default;
    };

    ObfuscatedTexture::Blur(texture_handle)
}

fn copy_link(i18n: &mut Localization, url: &str, img_resp: &Response) {
    img_resp.context_menu(|ui| {
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
fn render_media(
    ui: &mut egui::Ui,
    gifs: &mut GifStateMap,
    render_state: MediaRenderState,
    url: &str,
    size: egui::Vec2,
    i18n: &mut Localization,
    is_scaled: bool,
) -> egui::InnerResponse<Option<MediaUIAction>> {
    match render_state {
        MediaRenderState::ActualImage(image) => {
            let resp = render_success_media(ui, url, image, gifs, size, i18n, is_scaled);
            if resp.clicked() {
                egui::InnerResponse::new(Some(MediaUIAction::Clicked), resp)
            } else {
                egui::InnerResponse::new(None, resp)
            }
        }
        MediaRenderState::Transitioning { image, obfuscation } => match obfuscation {
            ObfuscatedTexture::Blur(texture) => {
                let resp = render_blur_transition(
                    ui,
                    url,
                    size,
                    texture,
                    image.get_first_texture(),
                    is_scaled,
                );
                if resp.inner {
                    egui::InnerResponse::new(Some(MediaUIAction::DoneLoading), resp.response)
                } else {
                    egui::InnerResponse::new(None, resp.response)
                }
            }
            ObfuscatedTexture::Default => {
                let scaled = ScaledTexture::new(image.get_first_texture(), size, is_scaled);
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
                shimmer_blurhash(texture_handle, ui, url, size, is_scaled),
            ),
            ObfuscatedTexture::Default => {
                let shimmer = true;
                egui::InnerResponse::new(
                    None,
                    render_default_blur_bg(ui, size, url, shimmer, is_scaled),
                )
            }
        },
        MediaRenderState::Obfuscated(obfuscated_texture) => {
            let resp = match obfuscated_texture {
                ObfuscatedTexture::Blur(texture_handle) => {
                    let scaled = ScaledTexture::new(texture_handle, size, is_scaled);

                    let resp = ui.add(scaled.get_image());
                    render_blur_text(ui, i18n, url, resp.rect)
                }
                ObfuscatedTexture::Default => render_default_blur(ui, i18n, size, url, is_scaled),
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
        egui::CornerRadius::same(8),
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

pub enum MediaRenderState<'a> {
    ActualImage(&'a mut TexturedImage),
    Transitioning {
        image: &'a mut TexturedImage,
        obfuscation: ObfuscatedTexture<'a>,
    },
    Error(&'a notedeck::Error),
    Shimmering(ObfuscatedTexture<'a>),
    Obfuscated(ObfuscatedTexture<'a>),
}

pub enum ObfuscatedTexture<'a> {
    Blur(&'a TextureHandle),
    Default,
}

/*
pub(crate) fn find_renderable_media<'a>(
    urls: &mut UrlMimes,
    imeta: &'a HashMap<String, ImageMetadata>,
    url: &'a str,
) -> Option<RenderableMedia> {
    let media_type = supported_mime_hosted_at_url(urls, url)?;

    let obfuscation_type = match imeta.get(url) {
        Some(blur) => ObfuscationType::Blurhash(blur.clone()),
        None => ObfuscationType::Default,
    };

    Some(RenderableMedia {
        url,
        media_type,
        obfuscation_type,
    })
}
*/

fn render_success_media(
    ui: &mut egui::Ui,
    url: &str,
    tex: &mut TexturedImage,
    gifs: &mut GifStateMap,
    size: Vec2,
    i18n: &mut Localization,
    is_scaled: bool,
) -> Response {
    let texture = ensure_latest_texture(ui, url, gifs, tex);

    let scaled = ScaledTexture::new(&texture, size, is_scaled);

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
    is_scaled: bool,
) -> egui::Response {
    let cur_alpha = get_blur_current_alpha(ui, url);

    let scaled = ScaledTexture::new(tex, size, is_scaled);
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

type FinishedTransition = bool;

// return true if transition is finished
fn render_blur_transition(
    ui: &mut egui::Ui,
    url: &str,
    size: Vec2,
    blur_texture: &TextureHandle,
    image_texture: &TextureHandle,
    is_scaled: bool,
) -> egui::InnerResponse<FinishedTransition> {
    let scaled_texture = ScaledTexture::new(image_texture, size, is_scaled);
    let scaled_blur_img = ScaledTexture::new(blur_texture, size, is_scaled);

    match get_blur_transition_state(ui.ctx(), url) {
        BlurTransitionState::StoppingShimmer { cur_alpha } => egui::InnerResponse::new(
            false,
            show_blurhash_with_alpha(ui, scaled_blur_img.get_image(), cur_alpha),
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

impl<'a> ScaledTexture<'a> {
    pub fn new(tex: &'a TextureHandle, max_size: Vec2, is_narrow: bool) -> Self {
        let tex_size = tex.size_vec2();

        let scaled_size = if !is_narrow {
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

    pub fn get_image(&self) -> Image {
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
