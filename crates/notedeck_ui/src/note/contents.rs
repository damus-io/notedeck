use std::{cell::OnceCell, collections::HashMap};

use crate::{
    blur::{imeta_blurhashes, Blur},
    contacts::trust_media_from_pk2,
    gif::{handle_repaint, retrieve_latest_texture},
    images::{render_images, ImageType},
    jobs::{BlurhashParams, Job, JobError, JobId, JobParams, JobParamsOwned, JobState, JobsCache},
    note::{NoteAction, NoteOptions, NoteResponse, NoteView},
    AnimationHelper,
};

use egui::{
    Button, Color32, CornerRadius, FontId, Hyperlink, Image, Response, RichText, Sense, Window,
};
use enostr::KeypairUnowned;
use nostrdb::{BlockType, Mention, Note, NoteKey, Transaction};
use tracing::warn;

use notedeck::{
    fonts::get_font_size, note::MediaAction, supported_mime_hosted_at_url, Images, JobPool,
    MediaCacheType, NoteContext, NotedeckTextStyle, UrlMimes,
};

pub struct NoteContents<'a, 'd> {
    note_context: &'a mut NoteContext<'d>,
    cur_acc: &'a Option<KeypairUnowned<'a>>,
    txn: &'a Transaction,
    note: &'a Note<'a>,
    options: NoteOptions,
    action: Option<NoteAction>,
    jobs: &'a mut JobsCache,
}

impl<'a, 'd> NoteContents<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        note_context: &'a mut NoteContext<'d>,
        cur_acc: &'a Option<KeypairUnowned<'a>>,
        txn: &'a Transaction,
        note: &'a Note,
        options: NoteOptions,
        jobs: &'a mut JobsCache,
    ) -> Self {
        NoteContents {
            note_context,
            cur_acc,
            txn,
            note,
            options,
            action: None,
            jobs,
        }
    }

    pub fn action(&self) -> &Option<NoteAction> {
        &self.action
    }
}

impl egui::Widget for &mut NoteContents<'_, '_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let result = render_note_contents(
            ui,
            self.note_context,
            self.cur_acc,
            self.txn,
            self.note,
            self.options,
            self.jobs,
        );
        self.action = result.action;
        result.response
    }
}

/// Render an inline note preview with a border. These are used when
/// notes are references within a note
#[allow(clippy::too_many_arguments)]
#[profiling::function]
pub fn render_note_preview(
    ui: &mut egui::Ui,
    note_context: &mut NoteContext,
    cur_acc: &Option<KeypairUnowned>,
    txn: &Transaction,
    id: &[u8; 32],
    parent: NoteKey,
    note_options: NoteOptions,
    jobs: &mut JobsCache,
) -> NoteResponse {
    let note = if let Ok(note) = note_context.ndb.get_note_by_id(txn, id) {
        // TODO: support other preview kinds
        if note.kind() == 1 {
            note
        } else {
            return NoteResponse::new(ui.colored_label(
                Color32::RED,
                format!("TODO: can't preview kind {}", note.kind()),
            ));
        }
    } else {
        return NoteResponse::new(ui.colored_label(Color32::RED, "TODO: COULD NOT LOAD"));
        /*
        return ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.colored_label(link_color, "@");
                ui.colored_label(link_color, &id_str[4..16]);
            })
            .response;
            */
    };

    egui::Frame::new()
        .fill(ui.visuals().noninteractive().weak_bg_fill)
        .inner_margin(egui::Margin::same(8))
        .outer_margin(egui::Margin::symmetric(0, 8))
        .corner_radius(egui::CornerRadius::same(10))
        .stroke(egui::Stroke::new(
            1.0,
            ui.visuals().noninteractive().bg_stroke.color,
        ))
        .show(ui, |ui| {
            NoteView::new(note_context, cur_acc, &note, note_options, jobs)
                .actionbar(false)
                .small_pfp(true)
                .wide(true)
                .note_previews(false)
                .options_button(true)
                .parent(parent)
                .is_preview(true)
                .show(ui)
        })
        .inner
}

#[allow(clippy::too_many_arguments)]
#[profiling::function]
pub fn render_note_contents(
    ui: &mut egui::Ui,
    note_context: &mut NoteContext,
    cur_acc: &Option<KeypairUnowned>,
    txn: &Transaction,
    note: &Note,
    options: NoteOptions,
    jobs: &mut JobsCache,
) -> NoteResponse {
    let note_key = note.key().expect("todo: implement non-db notes");
    let selectable = options.has_selectable_text();
    let mut note_action: Option<NoteAction> = None;
    let mut inline_note: Option<(&[u8; 32], &str)> = None;
    let hide_media = options.has_hide_media();
    let link_color = ui.visuals().hyperlink_color;

    if !options.has_is_preview() {
        // need this for the rect to take the full width of the column
        let _ = ui.allocate_at_least(egui::vec2(ui.available_width(), 0.0), egui::Sense::click());
    }

    let mut supported_medias: Vec<MediaRenderType> = vec![];
    let blurhashes = OnceCell::new();

    let response = ui.horizontal_wrapped(|ui| {
        let blocks = if let Ok(blocks) = note_context.ndb.get_blocks_by_key(txn, note_key) {
            blocks
        } else {
            warn!("missing note content blocks? '{}'", note.content());
            ui.weak(note.content());
            return;
        };

        let media_trusted = OnceCell::new();

        ui.spacing_mut().item_spacing.x = 0.0;

        for block in blocks.iter(note) {
            match block.blocktype() {
                BlockType::MentionBech32 => match block.as_mention().unwrap() {
                    Mention::Profile(profile) => {
                        let act = crate::Mention::new(
                            note_context.ndb,
                            note_context.img_cache,
                            txn,
                            profile.pubkey(),
                        )
                        .show(ui)
                        .inner;
                        if act.is_some() {
                            note_action = act;
                        }
                    }

                    Mention::Pubkey(npub) => {
                        let act = crate::Mention::new(
                            note_context.ndb,
                            note_context.img_cache,
                            txn,
                            npub.pubkey(),
                        )
                        .show(ui)
                        .inner;
                        if act.is_some() {
                            note_action = act;
                        }
                    }

                    Mention::Note(note) if options.has_note_previews() => {
                        inline_note = Some((note.id(), block.as_str()));
                    }

                    Mention::Event(note) if options.has_note_previews() => {
                        inline_note = Some((note.id(), block.as_str()));
                    }

                    _ => {
                        ui.colored_label(link_color, format!("@{}", &block.as_str()[4..16]));
                    }
                },

                BlockType::Hashtag => {
                    let resp = ui.colored_label(link_color, format!("#{}", block.as_str()));

                    if resp.clicked() {
                        note_action = Some(NoteAction::Hashtag(block.as_str().to_string()));
                    } else if resp.hovered() {
                        crate::show_pointer(ui);
                    }
                }

                BlockType::Url => {
                    let mut found_supported = || -> bool {
                        let url = block.as_str();

                        let blurs = blurhashes.get_or_init(|| imeta_blurhashes(note));

                        let trusted_media = media_trusted.get_or_init(|| {
                            trust_media_from_pk2(
                                note_context.ndb,
                                txn,
                                cur_acc.as_ref().map(|k| k.pubkey.bytes()),
                                note.pubkey(),
                            )
                        });

                        let Some(media_type) = find_supported_media_type(
                            ui,
                            &mut note_context.img_cache.urls,
                            blurs,
                            *trusted_media,
                            url,
                        ) else {
                            return false;
                        };

                        supported_medias.push(media_type);
                        true
                    };

                    if hide_media || !found_supported() {
                        ui.add(Hyperlink::from_label_and_url(
                            RichText::new(block.as_str()).color(link_color),
                            block.as_str(),
                        ));
                    }
                }

                BlockType::Text => {
                    if options.has_scramble_text() {
                        ui.add(egui::Label::new(rot13(block.as_str())).selectable(selectable));
                    } else {
                        ui.add(egui::Label::new(block.as_str()).selectable(selectable));
                    }
                }

                _ => {
                    ui.colored_label(link_color, block.as_str());
                }
            }
        }
    });

    let preview_note_action = if let Some((id, _block_str)) = inline_note {
        render_note_preview(ui, note_context, cur_acc, txn, id, note_key, options, jobs).action
    } else {
        None
    };

    let mut media_action = None;
    if !supported_medias.is_empty() && !options.has_textmode() {
        ui.add_space(2.0);
        let carousel_id = egui::Id::new(("carousel", note.key().expect("expected tx note")));

        media_action = image_carousel(
            ui,
            note_context.img_cache,
            note_context.job_pool,
            jobs,
            supported_medias,
            carousel_id,
        );
        ui.add_space(2.0);
    }

    let note_action = preview_note_action
        .or(note_action)
        .or(media_action.map(NoteAction::Media));

    NoteResponse::new(response.response).with_action(note_action)
}

fn find_supported_media_type<'a>(
    ui: &mut egui::Ui,
    urls: &mut UrlMimes,
    blurhashes: &'a HashMap<&'a str, Blur<'a>>,
    media_trusted: bool,
    url: &'a str,
) -> Option<MediaRenderType<'a>> {
    let media_type = supported_mime_hosted_at_url(urls, url)?;

    if blur_media(ui, url, media_trusted) {
        let blur_type = match blurhashes.get(url) {
            Some(blur) => BlurType::Blurhash(RenderableBlur { url, blur }),
            None => BlurType::Default(url),
        };
        Some(MediaRenderType::Untrusted(blur_type))
    } else {
        Some(MediaRenderType::Trusted(RenderableMedia {
            url,
            media_type,
        }))
    }
}

fn rot13(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() {
                // Rotate lowercase letters
                (((c as u8 - b'a' + 13) % 26) + b'a') as char
            } else if c.is_ascii_uppercase() {
                // Rotate uppercase letters
                (((c as u8 - b'A' + 13) % 26) + b'A') as char
            } else {
                // Leave other characters unchanged
                c
            }
        })
        .collect()
}

fn image_carousel(
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    job_pool: &mut JobPool,
    jobs: &mut JobsCache,
    medias: Vec<MediaRenderType>,
    carousel_id: egui::Id,
) -> Option<MediaAction> {
    // let's make sure everything is within our area

    let height = 360.0;
    let width = ui.available_size().x;
    let spinsz = if height > width { width } else { height };

    let show_popup = ui.ctx().memory(|mem| {
        mem.data
            .get_temp(carousel_id.with("show_popup"))
            .unwrap_or(false)
    });

    let current_image = 'scope: {
        if !show_popup {
            break 'scope None;
        }

        let MediaRenderType::Trusted(media) = &medias[0] else {
            break 'scope None;
        };

        Some(ui.ctx().memory(|mem| {
            mem.data
                .get_temp::<(String, MediaCacheType)>(carousel_id.with("current_image"))
                .unwrap_or_else(|| (media.url.to_owned(), media.media_type.clone()))
        }))
    };
    let mut action = None;

    ui.add_sized([width, height], |ui: &mut egui::Ui| {
        egui::ScrollArea::horizontal()
            .id_salt(carousel_id)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for media in medias {
                        if let Some(cur_action) = render_media(
                            ui,
                            img_cache,
                            job_pool,
                            jobs,
                            media,
                            width,
                            height,
                            spinsz,
                            carousel_id,
                        ) {
                            action = Some(cur_action)
                        }
                    }
                })
                .response
            })
            .inner
    });

    if show_popup {
        let current_image = current_image
            .as_ref()
            .expect("the image was actually clicked");
        let image = current_image.clone().0;
        let cache_type = current_image.clone().1;

        Window::new("image_popup")
            .title_bar(false)
            .fixed_size(ui.ctx().screen_rect().size())
            .fixed_pos(ui.ctx().screen_rect().min)
            .frame(egui::Frame::NONE)
            .show(ui.ctx(), |ui| {
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

                ui.centered_and_justified(|ui| {
                    render_images(
                        ui,
                        img_cache,
                        &image,
                        ImageType::Content,
                        cache_type.clone(),
                        |ui| {
                            ui.allocate_space(egui::vec2(spinsz, spinsz));
                        },
                        |ui, _| {
                            ui.allocate_space(egui::vec2(spinsz, spinsz));
                        },
                        |ui, url, renderable_media, gifs, _| {
                            let texture = handle_repaint(
                                ui,
                                retrieve_latest_texture(&image, gifs, renderable_media),
                            );

                            let texture_size = texture.size_vec2();
                            let screen_size = screen_rect.size();
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
                                0.5 - (visible_width / scaled_size.x) / 2.0
                                    + pan_offset.x / scaled_size.x,
                                0.5 - (visible_height / scaled_size.y) / 2.0
                                    + pan_offset.y / scaled_size.y,
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

                            copy_link(url, response);
                        },
                    );
                });
            });
    }
    action
}

fn copy_link(url: &str, img_resp: Response) {
    img_resp.context_menu(|ui| {
        if ui.button("Copy Link").clicked() {
            ui.ctx().copy_text(url.to_owned());
            ui.close_menu();
        }
    });
}

pub fn generate_blurhash_texturehandle(
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

#[allow(clippy::too_many_arguments)]
fn render_media(
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    job_pool: &mut JobPool,
    jobs: &mut JobsCache,
    media_type: MediaRenderType,
    width: f32,
    height: f32,
    spinsz: f32,
    carousel_id: egui::Id,
) -> Option<MediaAction> {
    match media_type {
        MediaRenderType::Trusted(renderable_media) => {
            render_image(
                ui,
                img_cache,
                renderable_media.url,
                renderable_media.media_type,
                height,
                spinsz,
                carousel_id,
            );
            None
        }
        MediaRenderType::Untrusted(blur_type) => match blur_type {
            BlurType::Blurhash(renderable_blur) => {
                let pixel_sizes = if let Some(media_size) = renderable_blur.blur.dimensions {
                    to_pixel_sizes(width, height, media_size.0, media_size.1)
                } else {
                    (width.round() as u32, height.round() as u32)
                };

                render_blurhash(ui, job_pool, jobs, &renderable_blur, pixel_sizes)
            }
            BlurType::Default(url) => {
                let resp = render_default_blur(ui, height, url);

                if resp.clicked() {
                    Some(MediaAction::Unblur(url.to_owned()))
                } else {
                    None
                }
            }
        },
    }
}

fn render_blur_text(ui: &mut egui::Ui, url: &str, render_rect: egui::Rect) -> egui::Response {
    let helper = AnimationHelper::new_from_rect(ui, ("show_media", url), render_rect);

    let painter = ui.painter_at(helper.get_animation_rect());

    let text_style = NotedeckTextStyle::Button;

    let icon_data = egui::include_image!("../../../../assets/icons/eye-slash-dark.png");

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

    egui::Image::new(icon_data)
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
    let (rect, _) = ui.allocate_exact_size(egui::vec2(height, height), egui::Sense::click());

    let painter = ui.painter_at(rect);

    painter.rect_filled(rect, CornerRadius::same(8), crate::colors::MID_GRAY);

    render_blur_text(ui, url, rect)
}

fn blur_media(ui: &mut egui::Ui, url: &str, media_trusted: bool) -> bool {
    !media_trusted && {
        let id = egui::Id::new(("blur", url));
        ui.ctx().data(|d| d.get_temp(id)).unwrap_or_else(|| {
            ui.ctx().data_mut(|d| d.insert_temp(id, true));
            true
        })
    }
}

fn to_pixel_sizes(
    window_width: f32,
    window_height: f32,
    media_width: u32,
    media_height: u32,
) -> (u32, u32) {
    let scale_x = window_width / media_width as f32;
    let scale_y = window_height / media_height as f32;
    let scale = scale_x.min(scale_y); // Use the smaller scale factor

    let new_width = (media_width as f32 * scale) as u32;
    let new_height = (media_height as f32 * scale) as u32;

    (new_width, new_height)
}

fn render_blurhash(
    ui: &mut egui::Ui,
    job_pool: &mut JobPool,
    jobs: &mut JobsCache,
    renderable_blur: &RenderableBlur,
    dims: (u32, u32),
) -> Option<MediaAction> {
    let params = BlurhashParams {
        blurhash: renderable_blur.blur.blurhash,
        url: renderable_blur.url,
        ctx: ui.ctx(),
    };

    let job_state = jobs.get_or_insert_with(
        job_pool,
        &JobId::Blurhash(renderable_blur.url),
        Some(JobParams::Blurhash(params)),
        move |params| compute_blurhash(params, dims),
    );

    let JobState::Completed(m_blur_job) = job_state else {
        return None;
    };

    #[allow(irrefutable_let_patterns)]
    let Job::Blurhash(m_texture_handle) = m_blur_job
    else {
        tracing::error!("Did not get the correct job type: {:?}", m_blur_job);
        return None;
    };

    let Some(texture_handle) = &m_texture_handle else {
        return None;
    };

    let resp = ui.add(
        Image::new(texture_handle)
            .max_height(dims.1 as f32)
            .corner_radius(5.0)
            .fit_to_original_size(1.0),
    );

    if render_blur_text(ui, renderable_blur.url, resp.rect)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        Some(MediaAction::Unblur(renderable_blur.url.to_owned()))
    } else {
        None
    }
}

fn compute_blurhash(params: Option<JobParamsOwned>, dims: (u32, u32)) -> Result<Job, JobError> {
    #[allow(irrefutable_let_patterns)]
    let Some(JobParamsOwned::Blurhash(params)) = params
    else {
        return Err(JobError::InvalidParameters);
    };

    let maybe_handle = match generate_blurhash_texturehandle(
        &params.ctx,
        &params.blurhash,
        &params.url,
        dims.0,
        dims.1,
    ) {
        Ok(tex) => Some(tex),
        Err(e) => {
            tracing::error!("failed to render blurhash: {e}");
            None
        }
    };

    Ok(Job::Blurhash(maybe_handle))
}

struct RenderableMedia<'a> {
    url: &'a str,
    media_type: MediaCacheType,
}

struct RenderableBlur<'a> {
    pub url: &'a str,
    pub blur: &'a Blur<'a>,
}

enum BlurType<'a> {
    Blurhash(RenderableBlur<'a>),
    Default(&'a str),
}

enum MediaRenderType<'a> {
    Trusted(RenderableMedia<'a>),
    Untrusted(BlurType<'a>),
}

fn render_image(
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    image: &str,
    cache_type: MediaCacheType,
    height: f32,
    spinsz: f32,
    carousel_id: egui::Id,
) {
    render_images(
        ui,
        img_cache,
        image,
        ImageType::Content,
        cache_type,
        |ui| {
            ui.add(egui::Spinner::new().size(spinsz));
        },
        |ui, _| {
            ui.allocate_space(egui::vec2(spinsz, spinsz));
        },
        |ui, url, renderable_media, gifs, cache_type| {
            let texture =
                handle_repaint(ui, retrieve_latest_texture(image, gifs, renderable_media));
            let img_resp = ui.add(
                Button::image(
                    Image::new(texture)
                        .max_height(height)
                        .corner_radius(5.0)
                        .fit_to_original_size(1.0),
                )
                .frame(false),
            );

            if img_resp.clicked() {
                ui.ctx().memory_mut(|mem| {
                    mem.data.insert_temp(carousel_id.with("show_popup"), true);
                    mem.data.insert_temp(
                        carousel_id.with("current_image"),
                        (url.to_owned(), cache_type.clone()),
                    );
                });
            }

            copy_link(url, img_resp);
        },
    );
}
