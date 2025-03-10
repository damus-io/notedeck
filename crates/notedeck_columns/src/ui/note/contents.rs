use std::cell::OnceCell;
use std::collections::HashMap;

use crate::blur::{imeta_blurhashes, Blur};
use crate::gif::{handle_repaint, retrieve_latest_texture};
use crate::images::generate_blurhash_texturehandle;
use crate::ui::images::render_images;
use crate::ui::{
    self,
    note::{NoteOptions, NoteResponse},
};
use crate::{actionbar::NoteAction, images::ImageType, timeline::TimelineKind};
use egui::{Button, Color32, Hyperlink, Image, Response, RichText, Sense, Spinner, Window};
use nostrdb::{BlockType, Mention, Ndb, Note, NoteKey, Transaction};
use tracing::warn;

use notedeck::{
    supported_mime_hosted_at_url, Images, Job, JobId, Jobs, MediaCacheType, NoteCache, UrlMimes,
};

/// Aggregates dependencies to reduce the number of parameters
/// passed to inner UI elements, minimizing prop drilling.
pub struct NoteContext<'d> {
    pub ndb: &'d Ndb,
    pub img_cache: &'d mut Images,
    pub note_cache: &'d mut NoteCache,
    pub jobs: &'d mut notedeck::Jobs,
}

pub struct NoteContents<'a, 'd> {
    note_context: &'a mut NoteContext<'d>,
    txn: &'a Transaction,
    note: &'a Note<'a>,
    options: NoteOptions,
    response: NoteContentsResponse,
}

impl<'a, 'd> NoteContents<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        note_context: &'a mut NoteContext<'d>,
        txn: &'a Transaction,
        note: &'a Note,
        options: ui::note::NoteOptions,
    ) -> Self {
        NoteContents {
            note_context,
            txn,
            note,
            options,
            response: Default::default(),
        }
    }

    pub fn action(&self) -> &Option<NoteAction> {
        &self.response.note_action
    }

    pub fn media_action(&self) -> &Option<MediaAction> {
        &self.response.media_action
    }
}

impl egui::Widget for &mut NoteContents<'_, '_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let (resp, result) =
            render_note_contents(ui, self.note_context, self.txn, self.note, self.options);
        self.response = result;
        resp
    }
}

/// Render an inline note preview with a border. These are used when
/// notes are references within a note
#[allow(clippy::too_many_arguments)]
pub fn render_note_preview(
    ui: &mut egui::Ui,
    note_context: &mut NoteContext,
    txn: &Transaction,
    id: &[u8; 32],
    parent: NoteKey,
    note_options: NoteOptions,
) -> NoteResponse {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

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
        .rounding(egui::Rounding::same(10))
        .stroke(egui::Stroke::new(
            1.0,
            ui.visuals().noninteractive().bg_stroke.color,
        ))
        .show(ui, |ui| {
            ui::NoteView::new(note_context, &note, note_options)
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

#[derive(Default)]
struct NoteContentsResponse {
    pub note_action: Option<NoteAction>,
    pub media_action: Option<MediaAction>,
}

#[allow(clippy::too_many_arguments)]
fn render_note_contents(
    ui: &mut egui::Ui,
    note_context: &mut NoteContext,
    txn: &Transaction,
    note: &Note,
    options: NoteOptions,
) -> (egui::Response, NoteContentsResponse) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

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

        let media_trusted = options.has_trusted_media();

        ui.spacing_mut().item_spacing.x = 0.0;

        for block in blocks.iter(note) {
            match block.blocktype() {
                BlockType::MentionBech32 => match block.as_mention().unwrap() {
                    Mention::Profile(profile) => {
                        let act = ui::Mention::new(
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
                        let act = ui::Mention::new(
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
                    #[cfg(feature = "profiling")]
                    puffin::profile_scope!("hashtag contents");
                    let resp = ui.colored_label(link_color, format!("#{}", block.as_str()));

                    if resp.clicked() {
                        note_action = Some(NoteAction::OpenTimeline(TimelineKind::Hashtag(
                            block.as_str().to_string(),
                        )));
                    } else if resp.hovered() {
                        ui::show_pointer(ui);
                    }
                }

                BlockType::Url => {
                    let mut found_supported = || -> bool {
                        let url = block.as_str();

                        let blurs = blurhashes.get_or_init(|| imeta_blurhashes(note));

                        let Some(media_type) = find_supported_media_type(
                            ui,
                            &mut note_context.img_cache.urls,
                            blurs,
                            media_trusted,
                            url,
                        ) else {
                            return false;
                        };

                        supported_medias.push(media_type);
                        true
                    };

                    if hide_media || !found_supported() {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("url contents");
                        ui.add(Hyperlink::from_label_and_url(
                            RichText::new(block.as_str()).color(link_color),
                            block.as_str(),
                        ));
                    }
                }

                BlockType::Text => {
                    #[cfg(feature = "profiling")]
                    puffin::profile_scope!("text contents");
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
        render_note_preview(ui, note_context, txn, id, note_key, options).action
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
            note_context.jobs,
            supported_medias,
            carousel_id,
        );
        ui.add_space(2.0);
    }

    let note_action = preview_note_action.or(note_action);

    let contents_response = NoteContentsResponse {
        note_action,
        media_action,
    };

    (response.response, contents_response)
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
        let blur = blurhashes.get(url)?;
        Some(MediaRenderType::Untrusted(RenderableBlur { url, blur }))
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
    jobs: &mut Jobs,
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

#[derive(Clone)]
pub enum MediaAction {
    Unblur(String),
}

impl MediaAction {
    pub fn process(&self, ui: &egui::Ui) {
        match &self {
            MediaAction::Unblur(url) => send_unblur_signal(ui.ctx(), url),
        }
    }
}

fn render_media(
    ui: &mut egui::Ui,
    img_cache: &mut Images,
    jobs: &mut Jobs,
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
        MediaRenderType::Untrusted(renderable_blur) => {
            let pixel_sizes = if let Some(media_size) = renderable_blur.blur.dimensions {
                to_pixel_sizes(width, height, media_size.0, media_size.1)
            } else {
                (width.round() as u32, height.round() as u32)
            };

            render_blurhash(ui, jobs, &renderable_blur, pixel_sizes)
        }
    }
}

fn send_unblur_signal(ctx: &egui::Context, url: &str) {
    let id = egui::Id::new(("blur", url));
    ctx.data_mut(|d| d.insert_temp(id, false))
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
    jobs: &mut Jobs,
    renderable_blur: &RenderableBlur,
    dims: (u32, u32),
) -> Option<MediaAction> {
    let jobid = JobId::Blurhash(renderable_blur.url.to_owned());
    let promise = jobs.jobs.entry(jobid).or_insert_with(|| {
        let (sender, promise) = poll_promise::Promise::new();
        let blurhash = renderable_blur.blur.blurhash.to_owned();
        let url = renderable_blur.url.to_owned();
        let ctx = ui.ctx().clone();
        std::thread::spawn(move || {
            let maybe_handle =
                match generate_blurhash_texturehandle(&ctx, &blurhash, &url, dims.0, dims.1) {
                    Ok(tex) => Some(tex),
                    Err(e) => {
                        tracing::error!("failed to render blurhash: {e}");
                        None
                    }
                };

            sender.send(Job::ProcessBlurhash(maybe_handle));
        });

        promise
    });

    let Some(Job::ProcessBlurhash(Some(texture_handle))) = promise.ready() else {
        return None;
    };

    let resp = ui.add(
        Image::new(texture_handle)
            .max_height(dims.1 as f32)
            .rounding(5.0)
            .fit_to_original_size(1.0),
    );

    // need click sense
    if ui
        .allocate_rect(resp.rect, egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        Some(MediaAction::Unblur(renderable_blur.url.to_owned()))
    } else {
        None
    }
}

struct RenderableMedia<'a> {
    url: &'a str,
    media_type: MediaCacheType,
}

struct RenderableBlur<'a> {
    pub url: &'a str,
    pub blur: &'a Blur<'a>,
}

enum MediaRenderType<'a> {
    Trusted(RenderableMedia<'a>),
    Untrusted(RenderableBlur<'a>),
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
            ui.add(Spinner::new().size(spinsz));
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
                        .rounding(5.0)
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
