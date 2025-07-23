use std::{collections::HashMap, path::Path};

use egui::{
    Button, Color32, Context, CornerRadius, FontId, Image, Response, RichText, Sense,
    TextureHandle, UiBuilder, Window,
};
use notedeck::{
    fonts::get_font_size, note::MediaAction, show_one_error_message, supported_mime_hosted_at_url,
    tr, GifState, GifStateMap, Images, JobPool, Localization, MediaCache, MediaCacheType,
    NotedeckTextStyle, TexturedImage, TexturesCache, UrlMimes,
};

use crate::{
    app_images,
    blur::{compute_blurhash, Blur, ObfuscationType, PointDimensions},
    colors::PINK,
    gif::{handle_repaint, retrieve_latest_texture},
    images::{fetch_no_pfp_promise, get_render_state, ImageType},
    jobs::{BlurhashParams, Job, JobId, JobParams, JobState, JobsCache},
    AnimationHelper, PulseAlpha,
};

#[allow(clippy::too_many_arguments)]
pub(crate) fn image_carousel(
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    job_pool: &mut JobPool,
    jobs: &mut JobsCache,
    medias: &[RenderableMedia],
    carousel_id: egui::Id,
    trusted_media: bool,
    i18n: &mut Localization,
) -> Option<MediaAction> {
    // let's make sure everything is within our area

    let height = 360.0;
    let width = ui.available_width();

    let show_popup = get_show_popup(ui, popup_id(carousel_id));
    let mut action = None;

    //let has_touch_screen = ui.ctx().input(|i| i.has_touch_screen());
    ui.add_sized([width, height], |ui: &mut egui::Ui| {
        egui::ScrollArea::horizontal()
            .drag_to_scroll(false)
            .id_salt(carousel_id)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
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
                            height,
                            &mut cache.textures_cache,
                            url,
                            *media_type,
                            &cache.cache_dir,
                            blur_type.clone(),
                        );

                        if let Some(cur_action) = render_media(
                            ui,
                            &mut img_cache.gif_states,
                            media_state,
                            url,
                            height,
                            i18n,
                        ) {
                            // clicked the media, lets set the active index
                            if let MediaUIAction::Clicked = cur_action {
                                set_show_popup(ui, popup_id(carousel_id), true);
                                set_selected_index(ui, selection_id(carousel_id), i);
                            }

                            action = cur_action.to_media_action(
                                ui.ctx(),
                                url,
                                *media_type,
                                cache,
                                ImageType::Content,
                            );
                        }
                    }
                })
                .response
            })
            .inner
    });

    if show_popup {
        if medias.is_empty() {
            return None;
        };

        let current_image_index = update_selected_image_index(ui, carousel_id, medias.len() as i32);

        show_full_screen_media(
            ui,
            medias,
            current_image_index,
            img_cache,
            carousel_id,
            i18n,
        );
    }
    action
}

enum MediaUIAction {
    Unblur,
    Error,
    DoneLoading,
    Clicked,
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
            MediaUIAction::Clicked => {
                tracing::debug!("{} clicked", url);
                None
            }

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
    medias: &[RenderableMedia],
    index: usize,
    img_cache: &mut Images,
    carousel_id: egui::Id,
    i18n: &mut Localization,
) {
    Window::new("image_popup")
        .title_bar(false)
        .fixed_size(ui.ctx().screen_rect().size())
        .fixed_pos(ui.ctx().screen_rect().min)
        .frame(egui::Frame::NONE)
        .show(ui.ctx(), |ui| {
            ui.centered_and_justified(|ui| 's: {
                let image_url = medias[index].url;

                let media_type = medias[index].media_type;
                tracing::trace!(
                    "show_full_screen_media using img {} @ {} for carousel_id {:?}",
                    image_url,
                    index,
                    carousel_id
                );

                let cur_state = get_render_state(
                    ui.ctx(),
                    img_cache,
                    media_type,
                    image_url,
                    ImageType::Content,
                );

                let notedeck::TextureState::Loaded(textured_image) = cur_state.texture_state else {
                    break 's;
                };

                render_full_screen_media(
                    ui,
                    medias.len(),
                    index,
                    textured_image,
                    cur_state.gifs,
                    image_url,
                    carousel_id,
                    i18n,
                );
            })
        });
}

fn set_selected_index(ui: &mut egui::Ui, sel_id: egui::Id, index: usize) {
    ui.data_mut(|d| {
        d.insert_temp(sel_id, index);
    });
}

fn get_selected_index(ui: &egui::Ui, selection_id: egui::Id) -> usize {
    ui.data(|d| d.get_temp(selection_id).unwrap_or(0))
}

/// Checks to see if we have any left/right key presses and updates the carousel index
fn update_selected_image_index(ui: &mut egui::Ui, carousel_id: egui::Id, num_urls: i32) -> usize {
    if num_urls > 1 {
        let next_image = ui.data(|data| {
            data.get_temp(carousel_id.with("next_image"))
                .unwrap_or(false)
        });
        let prev_image = ui.data(|data| {
            data.get_temp(carousel_id.with("prev_image"))
                .unwrap_or(false)
        });

        if next_image
            || ui.input(|i| i.key_pressed(egui::Key::ArrowRight) || i.key_pressed(egui::Key::L))
        {
            let ind = select_next_media(ui, carousel_id, num_urls, 1);
            tracing::debug!("carousel selecting right {}/{}", ind + 1, num_urls);
            if next_image {
                ui.data_mut(|data| data.remove_temp::<bool>(carousel_id.with("next_image")));
            }
            ind
        } else if prev_image
            || ui.input(|i| i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::H))
        {
            let ind = select_next_media(ui, carousel_id, num_urls, -1);
            tracing::debug!("carousel selecting left {}/{}", ind + 1, num_urls);
            if prev_image {
                ui.data_mut(|data| data.remove_temp::<bool>(carousel_id.with("prev_image")));
            }
            ind
        } else {
            get_selected_index(ui, selection_id(carousel_id))
        }
    } else {
        0
    }
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

// simple selector memory
fn select_next_media(
    ui: &mut egui::Ui,
    carousel_id: egui::Id,
    num_urls: i32,
    direction: i32,
) -> usize {
    let sel_id = selection_id(carousel_id);
    let current = get_selected_index(ui, sel_id) as i32;
    let next = current + direction;
    let next = if next >= num_urls {
        0
    } else if next < 0 {
        num_urls - 1
    } else {
        next
    };

    if next != current {
        set_selected_index(ui, sel_id, next as usize);
    }

    next as usize
}

#[allow(clippy::too_many_arguments)]
fn render_full_screen_media(
    ui: &mut egui::Ui,
    num_urls: usize,
    index: usize,
    renderable_media: &mut TexturedImage,
    gifs: &mut HashMap<String, GifState>,
    image_url: &str,
    carousel_id: egui::Id,
    i18n: &mut Localization,
) {
    const TOP_BAR_HEIGHT: f32 = 30.0;
    const BOTTOM_BAR_HEIGHT: f32 = 60.0;

    let screen_rect = ui.ctx().screen_rect();
    let screen_size = screen_rect.size();

    // Escape key closes popup
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.ctx().memory_mut(|mem| {
            mem.data.insert_temp(carousel_id.with("show_popup"), false);
        });
    }

    // Draw background
    ui.painter()
        .rect_filled(screen_rect, 0.0, Color32::from_black_alpha(230));

    let background_response = ui.interact(
        screen_rect,
        carousel_id.with("background"),
        egui::Sense::click(),
    );

    // Zoom & pan state
    let zoom_id = carousel_id.with("zoom_level");
    let pan_id = carousel_id.with("pan_offset");

    let mut zoom: f32 = ui
        .ctx()
        .memory(|mem| mem.data.get_temp(zoom_id).unwrap_or(1.0));
    let mut pan_offset = ui
        .ctx()
        .memory(|mem| mem.data.get_temp(pan_id).unwrap_or(egui::Vec2::ZERO));

    // Handle scroll to zoom
    if ui.input(|i| i.pointer.hover_pos()).is_some() {
        let scroll_delta = ui.input(|i| i.smooth_scroll_delta);
        if scroll_delta.y != 0.0 {
            let zoom_factor = if scroll_delta.y > 0.0 { 1.05 } else { 0.95 };
            zoom = (zoom * zoom_factor).clamp(0.1, 5.0);
            if zoom <= 1.0 {
                pan_offset = egui::Vec2::ZERO;
            }
            ui.ctx().memory_mut(|mem| {
                mem.data.insert_temp(zoom_id, zoom);
                mem.data.insert_temp(pan_id, pan_offset);
            });
        }
    }

    // Fetch image
    let texture = handle_repaint(
        ui,
        retrieve_latest_texture(image_url, gifs, renderable_media),
    );

    let texture_size = texture.size_vec2();

    let topbar_rect = egui::Rect::from_min_max(
        screen_rect.min + egui::vec2(0.0, 0.0),
        screen_rect.min + egui::vec2(screen_size.x, TOP_BAR_HEIGHT),
    );

    let topbar_response = ui.interact(
        topbar_rect,
        carousel_id.with("topbar"),
        egui::Sense::click(),
    );

    let mut keep_popup_open = false;
    if topbar_response.clicked() {
        keep_popup_open = true;
    }

    ui.allocate_new_ui(
        UiBuilder::new()
            .max_rect(topbar_rect)
            .layout(egui::Layout::top_down(egui::Align::RIGHT)),
        |ui| {
            let color = ui.style().visuals.noninteractive().fg_stroke.color;

            ui.add_space(10.0);

            ui.horizontal(|ui| {
                let label_reponse = ui
                    .label(RichText::new(image_url).color(color).small())
                    .on_hover_text(image_url);
                if label_reponse.double_clicked()
                    || label_reponse.clicked()
                    || label_reponse.hovered()
                {
                    keep_popup_open = true;

                    ui.ctx().copy_text(image_url.to_owned());
                }
            });
        },
    );

    // Calculate available rect for image
    let image_rect = egui::Rect::from_min_max(
        screen_rect.min + egui::vec2(0.0, TOP_BAR_HEIGHT),
        screen_rect.max - egui::vec2(0.0, BOTTOM_BAR_HEIGHT),
    );

    let image_area_size = image_rect.size();
    let scale = (image_area_size.x / texture_size.x)
        .min(image_area_size.y / texture_size.y)
        .min(1.0);
    let scaled_size = texture_size * scale * zoom;

    let visible_width = scaled_size.x.min(image_area_size.x);
    let visible_height = scaled_size.y.min(image_area_size.y);

    let max_pan_x = ((scaled_size.x - visible_width) / 2.0).max(0.0);
    let max_pan_y = ((scaled_size.y - visible_height) / 2.0).max(0.0);

    pan_offset.x = if max_pan_x > 0.0 {
        pan_offset.x.clamp(-max_pan_x, max_pan_x)
    } else {
        0.0
    };
    pan_offset.y = if max_pan_y > 0.0 {
        pan_offset.y.clamp(-max_pan_y, max_pan_y)
    } else {
        0.0
    };

    let render_rect = egui::Rect::from_center_size(
        image_rect.center(),
        egui::vec2(visible_width, visible_height),
    );

    // Compute UVs for zoom & pan
    let uv_min = egui::pos2(
        0.5 - (visible_width / scaled_size.x) / 2.0 + pan_offset.x / scaled_size.x,
        0.5 - (visible_height / scaled_size.y) / 2.0 + pan_offset.y / scaled_size.y,
    );
    let uv_max = egui::pos2(
        uv_min.x + visible_width / scaled_size.x,
        uv_min.y + visible_height / scaled_size.y,
    );

    // Paint image
    ui.painter().image(
        texture.id(),
        render_rect,
        egui::Rect::from_min_max(uv_min, uv_max),
        Color32::WHITE,
    );

    // image actions
    let response = ui.interact(
        render_rect,
        carousel_id.with("img"),
        Sense::click_and_drag(),
    );

    let swipe_accum_id = carousel_id.with("swipe_accum");
    let mut swipe_delta = ui.ctx().memory(|mem| {
        mem.data
            .get_temp::<egui::Vec2>(swipe_accum_id)
            .unwrap_or(egui::Vec2::ZERO)
    });

    // Handle pan via drag
    if response.dragged() {
        let delta = response.drag_delta();
        swipe_delta += delta;
        ui.ctx().memory_mut(|mem| {
            mem.data.insert_temp(swipe_accum_id, swipe_delta);
        });
        pan_offset -= delta;
        pan_offset.x = pan_offset.x.clamp(-max_pan_x, max_pan_x);
        pan_offset.y = pan_offset.y.clamp(-max_pan_y, max_pan_y);
        ui.ctx()
            .memory_mut(|mem| mem.data.insert_temp(pan_id, pan_offset));
    }

    // Double click to reset
    if response.double_clicked() {
        zoom = 1.0;
        pan_offset = egui::Vec2::ZERO;
        ui.ctx().memory_mut(|mem| {
            mem.data.insert_temp(pan_id, pan_offset);
            mem.data.insert_temp(zoom_id, zoom);
        });
    }

    let swipe_threshold = 50.0;
    if response.drag_stopped() {
        if swipe_delta.x.abs() > swipe_threshold && swipe_delta.y.abs() < swipe_threshold {
            if swipe_delta.x < 0.0 {
                ui.ctx().data_mut(|data| {
                    keep_popup_open = true;
                    data.insert_temp(carousel_id.with("next_image"), true);
                });
            } else if swipe_delta.x > 0.0 {
                ui.ctx().data_mut(|data| {
                    keep_popup_open = true;
                    data.insert_temp(carousel_id.with("prev_image"), true);
                });
            }
        }

        ui.ctx().memory_mut(|mem| {
            mem.data.remove::<egui::Vec2>(swipe_accum_id);
        });
    }

    // bottom bar
    if num_urls > 1 {
        let bottom_rect = egui::Rect::from_min_max(
            screen_rect.max - egui::vec2(screen_size.x, BOTTOM_BAR_HEIGHT),
            screen_rect.max,
        );

        let full_response = ui.interact(
            bottom_rect,
            carousel_id.with("bottom_bar"),
            egui::Sense::click(),
        );

        if full_response.clicked() {
            keep_popup_open = true;
        }

        let mut clicked_index: Option<usize> = None;

        #[allow(deprecated)]
        ui.allocate_ui_at_rect(bottom_rect, |ui| {
            let dot_radius = 7.0;
            let dot_spacing = 20.0;
            let color_active = PINK;
            let color_inactive: Color32 = ui.style().visuals.widgets.inactive.bg_fill;

            let center = bottom_rect.center();

            for i in 0..num_urls {
                let distance = egui::vec2(
                    (i as f32 - (num_urls as f32 - 1.0) / 2.0) * dot_spacing,
                    0.0,
                );
                let pos = center + distance;

                let circle_color = if i == index {
                    color_active
                } else {
                    color_inactive
                };

                let circle_rect = egui::Rect::from_center_size(
                    pos,
                    egui::vec2(dot_radius * 2.0, dot_radius * 2.0),
                );

                let resp = ui.interact(circle_rect, carousel_id.with(i), egui::Sense::click());

                ui.painter().circle_filled(pos, dot_radius, circle_color);

                if i != index && resp.hovered() {
                    ui.painter()
                        .circle_stroke(pos, dot_radius + 2.0, (1.0, PINK));
                }

                if resp.clicked() {
                    keep_popup_open = true;
                    if i != index {
                        clicked_index = Some(i);
                    }
                }
            }
        });

        if let Some(new_index) = clicked_index {
            ui.ctx().data_mut(|data| {
                data.insert_temp(selection_id(carousel_id), new_index);
            });
        }
    }

    if keep_popup_open || response.clicked() {
        ui.data_mut(|data| {
            data.insert_temp(carousel_id.with("show_popup"), true);
        });
    } else if background_response.clicked() || response.clicked_elsewhere() {
        ui.data_mut(|data| {
            data.insert_temp(carousel_id.with("show_popup"), false);
        });
    }

    copy_link(i18n, image_url, &response);
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
    height: f32,
    i18n: &mut Localization,
) -> Option<MediaUIAction> {
    match render_state {
        MediaRenderState::ActualImage(image) => {
            if render_success_media(ui, url, image, gifs, height, i18n).clicked() {
                Some(MediaUIAction::Clicked)
            } else {
                None
            }
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
                    render_blur_text(ui, i18n, url, resp.rect)
                }
                ObfuscatedTexture::Default => render_default_blur(ui, i18n, height, url),
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
    height: f32,
    url: &str,
) -> egui::Response {
    let rect = render_default_blur_bg(ui, height, url, false);
    render_blur_text(ui, i18n, url, rect)
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
        Some(blur) => ObfuscationType::Blurhash(blur.clone()),
        None => ObfuscationType::Default,
    };

    Some(RenderableMedia {
        url,
        media_type,
        obfuscation_type,
    })
}

#[inline]
fn selection_id(carousel_id: egui::Id) -> egui::Id {
    carousel_id.with("sel")
}

/// get the popup carousel window state
#[inline]
fn get_show_popup(ui: &egui::Ui, popup_id: egui::Id) -> bool {
    ui.data(|data| data.get_temp(popup_id).unwrap_or(false))
}

/// set the popup carousel window state
#[inline]
fn set_show_popup(ui: &mut egui::Ui, popup_id: egui::Id, show_popup: bool) {
    ui.data_mut(|data| data.insert_temp(popup_id, show_popup));
}

#[inline]
fn popup_id(carousel_id: egui::Id) -> egui::Id {
    carousel_id.with("show_popup")
}

fn render_success_media(
    ui: &mut egui::Ui,
    url: &str,
    tex: &mut TexturedImage,
    gifs: &mut GifStateMap,
    height: f32,
    i18n: &mut Localization,
) -> Response {
    let texture = handle_repaint(ui, retrieve_latest_texture(url, gifs, tex));
    let img = texture_to_image(texture, height);
    let img_resp = ui.add(Button::image(img).frame(false));

    copy_link(i18n, url, &img_resp);

    img_resp
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
