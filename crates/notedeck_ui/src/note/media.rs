use std::{collections::HashMap, path::Path};

use egui::{
    Button, Color32, Context, CornerRadius, FontId, Image, Response, Sense, TextureHandle, Window,
};
use notedeck::{
    fonts::get_font_size, note::MediaAction, show_one_error_message, supported_mime_hosted_at_url,
    GifState, GifStateMap, Images, JobPool, MediaCache, MediaCacheType, NotedeckTextStyle,
    TexturedImage, TexturesCache, UrlMimes,
};

use crate::{
    app_images,
    blur::{compute_blurhash, Blur, ObfuscationType, PointDimensions},
    gif::{handle_repaint, retrieve_latest_texture},
    images::{fetch_no_pfp_promise, get_render_state, ImageType},
    jobs::{BlurhashParams, Job, JobId, JobParams, JobState, JobsCache},
    AnimationHelper, PulseAlpha,
};

pub(crate) fn image_carousel(
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    job_pool: &mut JobPool,
    jobs: &mut JobsCache,
    medias: Vec<RenderableMedia>,
    carousel_id: egui::Id,
    trusted_media: bool,
) -> Option<MediaAction> {
    // let's make sure everything is within our area

    let height = 360.0;
    let width = ui.available_width();

    let show_popup = ui.ctx().memory(|mem| {
        mem.data
            .get_temp(carousel_id.with("show_popup"))
            .unwrap_or(false)
    });

    let current_image = 'scope: {
        if !show_popup {
            break 'scope None;
        }

        let Some(media) = medias.first() else {
            break 'scope None;
        };

        Some(ui.ctx().memory(|mem| {
            mem.data
                .get_temp::<(String, MediaCacheType)>(carousel_id.with("current_image"))
                .unwrap_or_else(|| (media.url.to_owned(), media.media_type))
        }))
    };
    let mut action = None;

    //let has_touch_screen = ui.ctx().input(|i| i.has_touch_screen());

    ui.add_sized([width, height], |ui: &mut egui::Ui| {
        egui::ScrollArea::horizontal()
            .drag_to_scroll(false)
            .id_salt(carousel_id)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for media in medias {
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
                            height,
                            &mut cache.textures_cache,
                            url,
                            media_type,
                            &cache.cache_dir,
                            blur_type,
                        );
                        if let Some(cur_action) = render_media(
                            ui,
                            &mut img_cache.gif_states,
                            media_state,
                            url,
                            media_type,
                            height,
                            carousel_id,
                        ) {
                            let cur_action = cur_action.to_media_action(
                                ui.ctx(),
                                url,
                                media_type,
                                cache,
                                ImageType::Content,
                            );
                            if let Some(cur_action) = cur_action {
                                action = Some(cur_action);
                            }
                        }
                    }
                })
                .response
            })
            .inner
    });

    if show_popup {
        if let Some((image_url, cache_type)) = current_image {
            show_full_screen_media(ui, &image_url, cache_type, img_cache, carousel_id);
        }
    }
    action
}

enum MediaUIAction {
    Unblur,
    Error,
    DoneLoading,
}

impl MediaUIAction {
    pub fn to_media_action(
        &self,
        ctx: &egui::Context,
        url: &str,
        cache_type: MediaCacheType,
        cache: &mut MediaCache,
        img_type: ImageType,
    ) -> Option<MediaAction> {
        match self {
            MediaUIAction::Unblur => Some(MediaAction::FetchImage {
                url: url.to_owned(),
                cache_type,
                no_pfp_promise: crate::images::fetch_img(
                    &cache.cache_dir,
                    ctx,
                    url,
                    img_type,
                    cache_type,
                ),
            }),
            MediaUIAction::Error => {
                if !matches!(img_type, ImageType::Profile(_)) {
                    return None;
                };

                Some(MediaAction::FetchImage {
                    url: url.to_owned(),
                    cache_type,
                    no_pfp_promise: fetch_no_pfp_promise(ctx, cache),
                })
            }
            MediaUIAction::DoneLoading => Some(MediaAction::DoneLoading {
                url: url.to_owned(),
                cache_type,
            }),
        }
    }
}

fn show_full_screen_media(
    ui: &mut egui::Ui,
    image_url: &str,
    cache_type: MediaCacheType,
    img_cache: &mut Images,
    carousel_id: egui::Id,
) {
    Window::new("image_popup")
        .title_bar(false)
        .fixed_size(ui.ctx().screen_rect().size())
        .fixed_pos(ui.ctx().screen_rect().min)
        .frame(egui::Frame::NONE)
        .show(ui.ctx(), |ui| {
            ui.centered_and_justified(|ui| 's: {
                let cur_state = get_render_state(
                    ui.ctx(),
                    img_cache,
                    cache_type,
                    image_url,
                    ImageType::Content,
                );

                let notedeck::TextureState::Loaded(textured_image) = cur_state.texture_state else {
                    break 's;
                };

                render_full_screen_media(
                    ui,
                    textured_image,
                    cur_state.gifs,
                    image_url,
                    carousel_id,
                );
            })
        });
}

#[allow(clippy::too_many_arguments)]
pub fn get_content_media_render_state<'a>(
    ui: &mut egui::Ui,
    job_pool: &'a mut JobPool,
    jobs: &'a mut JobsCache,
    media_trusted: bool,
    height: f32,
    cache: &'a mut TexturesCache,
    url: &'a str,
    cache_type: MediaCacheType,
    cache_dir: &Path,
    obfuscation_type: ObfuscationType<'a>,
) -> MediaRenderState<'a> {
    let render_type = if media_trusted {
        cache.handle_and_get_or_insert_loadable(url, || {
            crate::images::fetch_img(cache_dir, ui.ctx(), url, ImageType::Content, cache_type)
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
            height,
        ));
    };

    match render_type {
        notedeck::LoadableTextureState::Pending => MediaRenderState::Shimmering(get_obfuscated(
            ui,
            url,
            obfuscation_type,
            job_pool,
            jobs,
            height,
        )),
        notedeck::LoadableTextureState::Error(e) => MediaRenderState::Error(e),
        notedeck::LoadableTextureState::Loading { actual_image_tex } => {
            let obfuscation = get_obfuscated(ui, url, obfuscation_type, job_pool, jobs, height);
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
    obfuscation_type: ObfuscationType<'a>,
    job_pool: &'a mut JobPool,
    jobs: &'a mut JobsCache,
    height: f32,
) -> ObfuscatedTexture<'a> {
    let ObfuscationType::Blurhash(renderable_blur) = obfuscation_type else {
        return ObfuscatedTexture::Default;
    };

    let params = BlurhashParams {
        blurhash: renderable_blur.blurhash,
        url,
        ctx: ui.ctx(),
    };

    let available_points = PointDimensions {
        x: ui.available_width(),
        y: height,
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

fn render_full_screen_media(
    ui: &mut egui::Ui,
    renderable_media: &mut TexturedImage,
    gifs: &mut HashMap<String, GifState>,
    image_url: &str,
    carousel_id: egui::Id,
) {
    let screen_rect = ui.ctx().screen_rect();

    // escape
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.ctx().memory_mut(|mem| {
            mem.data.insert_temp(carousel_id.with("show_popup"), false);
        });
    }

    // background
    ui.painter()
        .rect_filled(screen_rect, 0.0, Color32::from_black_alpha(230));

    // zoom init
    let zoom_id = carousel_id.with("zoom_level");
    let mut zoom = ui
        .ctx()
        .memory(|mem| mem.data.get_temp(zoom_id).unwrap_or(1.0_f32));

    // pan init
    let pan_id = carousel_id.with("pan_offset");
    let mut pan_offset = ui
        .ctx()
        .memory(|mem| mem.data.get_temp(pan_id).unwrap_or(egui::Vec2::ZERO));

    // zoom & scroll
    if ui.input(|i| i.pointer.hover_pos()).is_some() {
        let scroll_delta = ui.input(|i| i.smooth_scroll_delta);
        if scroll_delta.y != 0.0 {
            let zoom_factor = if scroll_delta.y > 0.0 { 1.05 } else { 0.95 };
            zoom *= zoom_factor;
            zoom = zoom.clamp(0.1, 5.0);

            if zoom <= 1.0 {
                pan_offset = egui::Vec2::ZERO;
            }

            ui.ctx().memory_mut(|mem| {
                mem.data.insert_temp(zoom_id, zoom);
                mem.data.insert_temp(pan_id, pan_offset);
            });
        }
    }

    let texture = handle_repaint(
        ui,
        retrieve_latest_texture(image_url, gifs, renderable_media),
    );

    let texture_size = texture.size_vec2();
    let screen_size = ui.ctx().screen_rect().size();
    let scale = (screen_size.x / texture_size.x)
        .min(screen_size.y / texture_size.y)
        .min(1.0);
    let scaled_size = texture_size * scale * zoom;

    let visible_width = scaled_size.x.min(screen_size.x);
    let visible_height = scaled_size.y.min(screen_size.y);

    let max_pan_x = ((scaled_size.x - visible_width) / 2.0).max(0.0);
    let max_pan_y = ((scaled_size.y - visible_height) / 2.0).max(0.0);

    if max_pan_x > 0.0 {
        pan_offset.x = pan_offset.x.clamp(-max_pan_x, max_pan_x);
    } else {
        pan_offset.x = 0.0;
    }

    if max_pan_y > 0.0 {
        pan_offset.y = pan_offset.y.clamp(-max_pan_y, max_pan_y);
    } else {
        pan_offset.y = 0.0;
    }

    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(visible_width, visible_height),
        egui::Sense::click_and_drag(),
    );

    let uv_min = egui::pos2(
        0.5 - (visible_width / scaled_size.x) / 2.0 + pan_offset.x / scaled_size.x,
        0.5 - (visible_height / scaled_size.y) / 2.0 + pan_offset.y / scaled_size.y,
    );

    let uv_max = egui::pos2(
        uv_min.x + visible_width / scaled_size.x,
        uv_min.y + visible_height / scaled_size.y,
    );

    let uv = egui::Rect::from_min_max(uv_min, uv_max);

    ui.painter()
        .image(texture.id(), rect, uv, egui::Color32::WHITE);
    let img_rect = ui.allocate_rect(rect, Sense::click());

    if img_rect.clicked() {
        ui.ctx().memory_mut(|mem| {
            mem.data.insert_temp(carousel_id.with("show_popup"), true);
        });
    } else if img_rect.clicked_elsewhere() {
        ui.ctx().memory_mut(|mem| {
            mem.data.insert_temp(carousel_id.with("show_popup"), false);
        });
    }

    // Handle dragging for pan
    if response.dragged() {
        let delta = response.drag_delta();

        pan_offset.x -= delta.x;
        pan_offset.y -= delta.y;

        if max_pan_x > 0.0 {
            pan_offset.x = pan_offset.x.clamp(-max_pan_x, max_pan_x);
        } else {
            pan_offset.x = 0.0;
        }

        if max_pan_y > 0.0 {
            pan_offset.y = pan_offset.y.clamp(-max_pan_y, max_pan_y);
        } else {
            pan_offset.y = 0.0;
        }

        ui.ctx().memory_mut(|mem| {
            mem.data.insert_temp(pan_id, pan_offset);
        });
    }

    // reset zoom on double-click
    if response.double_clicked() {
        pan_offset = egui::Vec2::ZERO;
        zoom = 1.0;
        ui.ctx().memory_mut(|mem| {
            mem.data.insert_temp(pan_id, pan_offset);
            mem.data.insert_temp(zoom_id, zoom);
        });
    }

    copy_link(image_url, response);
}

fn copy_link(url: &str, img_resp: Response) {
    img_resp.context_menu(|ui| {
        if ui.button("Copy Link").clicked() {
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
    cache_type: MediaCacheType,
    height: f32,
    carousel_id: egui::Id,
) -> Option<MediaUIAction> {
    match render_state {
        MediaRenderState::ActualImage(image) => {
            render_success_media(ui, url, image, gifs, cache_type, height, carousel_id);
            None
        }
        MediaRenderState::Transitioning { image, obfuscation } => match obfuscation {
            ObfuscatedTexture::Blur(texture) => {
                if render_blur_transition(ui, url, height, texture, image.get_first_texture()) {
                    Some(MediaUIAction::DoneLoading)
                } else {
                    None
                }
            }
            ObfuscatedTexture::Default => {
                ui.add(texture_to_image(image.get_first_texture(), height));
                Some(MediaUIAction::DoneLoading)
            }
        },
        MediaRenderState::Error(e) => {
            ui.allocate_space(egui::vec2(height, height));
            show_one_error_message(ui, &format!("Could not render media {url}: {e}"));
            Some(MediaUIAction::Error)
        }
        MediaRenderState::Shimmering(obfuscated_texture) => {
            match obfuscated_texture {
                ObfuscatedTexture::Blur(texture_handle) => {
                    shimmer_blurhash(texture_handle, ui, url, height);
                }
                ObfuscatedTexture::Default => {
                    render_default_blur_bg(ui, height, url, true);
                }
            }
            None
        }
        MediaRenderState::Obfuscated(obfuscated_texture) => {
            let resp = match obfuscated_texture {
                ObfuscatedTexture::Blur(texture_handle) => {
                    let resp = ui.add(texture_to_image(texture_handle, height));
                    render_blur_text(ui, url, resp.rect)
                }
                ObfuscatedTexture::Default => render_default_blur(ui, height, url),
            };

            if resp
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                Some(MediaUIAction::Unblur)
            } else {
                None
            }
        }
    }
}

fn render_blur_text(ui: &mut egui::Ui, url: &str, render_rect: egui::Rect) -> egui::Response {
    let helper = AnimationHelper::new_from_rect(ui, ("show_media", url), render_rect);

    let painter = ui.painter_at(helper.get_animation_rect());

    let text_style = NotedeckTextStyle::Button;

    let icon_size = helper.scale_1d_pos(30.0);
    let animation_fontid = FontId::new(
        helper.scale_1d_pos(get_font_size(ui.ctx(), &text_style)),
        text_style.font_family(),
    );
    let info_galley = painter.layout(
        "Media from someone you don't follow".to_owned(),
        animation_fontid.clone(),
        ui.visuals().text_color(),
        render_rect.width() / 2.0,
    );

    let load_galley = painter.layout_no_wrap(
        "Tap to Load".to_owned(),
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

fn render_default_blur(ui: &mut egui::Ui, height: f32, url: &str) -> egui::Response {
    let rect = render_default_blur_bg(ui, height, url, false);
    render_blur_text(ui, url, rect)
}

fn render_default_blur_bg(ui: &mut egui::Ui, height: f32, url: &str, shimmer: bool) -> egui::Rect {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(height, height), egui::Sense::click());

    let painter = ui.painter_at(rect);

    let mut color = crate::colors::MID_GRAY;
    if shimmer {
        let [r, g, b, _a] = color.to_srgba_unmultiplied();
        let cur_alpha = get_blur_current_alpha(ui, url);
        color = Color32::from_rgba_unmultiplied(r, g, b, cur_alpha)
    }

    painter.rect_filled(rect, CornerRadius::same(8), color);

    rect
}

pub(crate) struct RenderableMedia<'a> {
    url: &'a str,
    media_type: MediaCacheType,
    obfuscation_type: ObfuscationType<'a>,
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

pub(crate) fn find_renderable_media<'a>(
    urls: &mut UrlMimes,
    blurhashes: &'a HashMap<&'a str, Blur<'a>>,
    url: &'a str,
) -> Option<RenderableMedia<'a>> {
    let media_type = supported_mime_hosted_at_url(urls, url)?;

    let obfuscation_type = match blurhashes.get(url) {
        Some(blur) => ObfuscationType::Blurhash(blur),
        None => ObfuscationType::Default,
    };

    Some(RenderableMedia {
        url,
        media_type,
        obfuscation_type,
    })
}

fn render_success_media(
    ui: &mut egui::Ui,
    url: &str,
    tex: &mut TexturedImage,
    gifs: &mut GifStateMap,
    cache_type: MediaCacheType,
    height: f32,
    carousel_id: egui::Id,
) {
    let texture = handle_repaint(ui, retrieve_latest_texture(url, gifs, tex));
    let img = texture_to_image(texture, height);
    let img_resp = ui.add(Button::image(img).frame(false));

    if img_resp.clicked() {
        ui.ctx().memory_mut(|mem| {
            mem.data.insert_temp(carousel_id.with("show_popup"), true);
            mem.data.insert_temp(
                carousel_id.with("current_image"),
                (url.to_owned(), cache_type),
            );
        });
    }

    copy_link(url, img_resp);
}

fn texture_to_image(tex: &TextureHandle, max_height: f32) -> egui::Image {
    Image::new(tex)
        .max_height(max_height)
        .corner_radius(5.0)
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

fn shimmer_blurhash(tex: &TextureHandle, ui: &mut egui::Ui, url: &str, max_height: f32) {
    let cur_alpha = get_blur_current_alpha(ui, url);

    let scaled = ScaledTexture::new(tex, max_height);
    let img = scaled.get_image();
    show_blurhash_with_alpha(ui, img, cur_alpha);
}

fn fade_color(alpha: u8) -> egui::Color32 {
    Color32::from_rgba_unmultiplied(255, 255, 255, alpha)
}

fn show_blurhash_with_alpha(ui: &mut egui::Ui, img: Image, alpha: u8) {
    let cur_color = fade_color(alpha);

    let img = img.tint(cur_color);

    ui.add(img);
}

type FinishedTransition = bool;

// return true if transition is finished
fn render_blur_transition(
    ui: &mut egui::Ui,
    url: &str,
    max_height: f32,
    blur_texture: &TextureHandle,
    image_texture: &TextureHandle,
) -> FinishedTransition {
    let scaled_texture = ScaledTexture::new(image_texture, max_height);

    let blur_img = texture_to_image(blur_texture, max_height);
    match get_blur_transition_state(ui.ctx(), url) {
        BlurTransitionState::StoppingShimmer { cur_alpha } => {
            show_blurhash_with_alpha(ui, blur_img, cur_alpha);
            false
        }
        BlurTransitionState::FadingBlur => render_blur_fade(ui, url, blur_img, &scaled_texture),
    }
}

struct ScaledTexture<'a> {
    tex: &'a TextureHandle,
    max_height: f32,
    pub scaled_size: egui::Vec2,
}

impl<'a> ScaledTexture<'a> {
    pub fn new(tex: &'a TextureHandle, max_height: f32) -> Self {
        let scaled_size = {
            let mut size = tex.size_vec2();

            if size.y > max_height {
                let old_y = size.y;
                size.y = max_height;
                size.x *= max_height / old_y;
            }

            size
        };

        Self {
            tex,
            max_height,
            scaled_size,
        }
    }

    pub fn get_image(&self) -> Image {
        texture_to_image(self.tex, self.max_height)
            .max_size(self.scaled_size)
            .shrink_to_fit()
    }
}

fn render_blur_fade(
    ui: &mut egui::Ui,
    url: &str,
    blur_img: Image,
    image_texture: &ScaledTexture,
) -> FinishedTransition {
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

    let (rect, _) = ui.allocate_exact_size(alloc_size, egui::Sense::hover());

    img.paint_at(ui, rect);
    blur_img.paint_at(ui, rect);

    cur_alpha == 0
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
