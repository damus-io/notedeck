use crate::{
    gif::{handle_repaint, retrieve_latest_texture},
    images::{render_images, ImageType},
    note::{NoteAction, NoteOptions, NoteResponse, NoteView},
};

use egui::{Button, Color32, Hyperlink, Image, Response, RichText, Sense, Window};
use enostr::KeypairUnowned;
use nostrdb::{BlockType, Mention, Note, NoteKey, Transaction};
use tracing::warn;

use notedeck::{supported_mime_hosted_at_url, Images, MediaCacheType, NoteContext};

pub struct NoteContents<'a, 'd> {
    note_context: &'a mut NoteContext<'d>,
    cur_acc: &'a Option<KeypairUnowned<'a>>,
    txn: &'a Transaction,
    note: &'a Note<'a>,
    options: NoteOptions,
    pub action: Option<NoteAction>,
}

impl<'a, 'd> NoteContents<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        note_context: &'a mut NoteContext<'d>,
        cur_acc: &'a Option<KeypairUnowned<'a>>,
        txn: &'a Transaction,
        note: &'a Note,
        options: NoteOptions,
    ) -> Self {
        NoteContents {
            note_context,
            cur_acc,
            txn,
            note,
            options,
            action: None,
        }
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

    NoteView::new(note_context, cur_acc, &note, note_options)
        .preview_style()
        .parent(parent)
        .show(ui)
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
) -> NoteResponse {
    let note_key = note.key().expect("todo: implement non-db notes");
    let selectable = options.has_selectable_text();
    let mut images: Vec<(String, MediaCacheType)> = vec![];
    let mut note_action: Option<NoteAction> = None;
    let mut inline_note: Option<(&[u8; 32], &str)> = None;
    let hide_media = options.has_hide_media();
    let link_color = ui.visuals().hyperlink_color;

    // The current length of the rendered blocks. Used in trucation logic
    let mut current_len: usize = 0;
    let truncate_len = 280;

    if !options.has_is_preview() {
        // need this for the rect to take the full width of the column
        let _ = ui.allocate_at_least(egui::vec2(ui.available_width(), 0.0), egui::Sense::click());
    }

    let response = ui.horizontal_wrapped(|ui| {
        let blocks = if let Ok(blocks) = note_context.ndb.get_blocks_by_key(txn, note_key) {
            blocks
        } else {
            warn!("missing note content blocks? '{}'", note.content());
            ui.weak(note.content());
            return;
        };

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
                        if let Some(cache_type) =
                            supported_mime_hosted_at_url(&mut note_context.img_cache.urls, url)
                        {
                            images.push((url.to_string(), cache_type));
                            true
                        } else {
                            false
                        }
                    };
                    if hide_media || !found_supported() {
                        ui.add(Hyperlink::from_label_and_url(
                            RichText::new(block.as_str()).color(link_color),
                            block.as_str(),
                        ));
                    }
                }

                BlockType::Text => {
                    // truncate logic
                    let mut truncate = false;
                    let block_str = if options.has_truncate()
                        && (current_len + block.as_str().len() > truncate_len)
                    {
                        truncate = true;
                        // The current block goes over the truncate length,
                        // we'll need to truncate this block
                        let block_str = block.as_str();
                        let closest = notedeck::abbrev::floor_char_boundary(
                            block_str,
                            truncate_len - current_len,
                        );
                        &(block_str[..closest].to_string() + "…")
                    } else {
                        let block_str = block.as_str();
                        current_len += block_str.len();
                        block_str
                    };

                    if options.has_scramble_text() {
                        ui.add(
                            egui::Label::new(rot13(block_str))
                                .wrap()
                                .selectable(selectable),
                        );
                    } else {
                        ui.add(egui::Label::new(block_str).wrap().selectable(selectable));
                    }

                    // don't render any more blocks
                    if truncate {
                        break;
                    }
                }

                _ => {
                    ui.colored_label(link_color, block.as_str());
                }
            }
        }
    });

    let preview_note_action = if let Some((id, _block_str)) = inline_note {
        render_note_preview(ui, note_context, cur_acc, txn, id, note_key, options).action
    } else {
        None
    };

    if !images.is_empty() && !options.has_textmode() {
        ui.add_space(2.0);
        let carousel_id = egui::Id::new(("carousel", note.key().expect("expected tx note")));
        image_carousel(ui, note_context.img_cache, images, carousel_id);
        ui.add_space(2.0);
    }

    let note_action = preview_note_action.or(note_action);

    NoteResponse::new(response.response).with_action(note_action)
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
    images: Vec<(String, MediaCacheType)>,
    carousel_id: egui::Id,
) {
    // let's make sure everything is within our area

    let height = 360.0;
    let width = ui.available_size().x;
    let spinsz = if height > width { width } else { height };

    let show_popup = ui.ctx().memory(|mem| {
        mem.data
            .get_temp(carousel_id.with("show_popup"))
            .unwrap_or(false)
    });

    let current_image = show_popup.then(|| {
        ui.ctx().memory(|mem| {
            mem.data
                .get_temp::<(String, MediaCacheType)>(carousel_id.with("current_image"))
                .unwrap_or_else(|| (images[0].0.clone(), images[0].1.clone()))
        })
    });

    ui.add_sized([width, height], |ui: &mut egui::Ui| {
        egui::ScrollArea::horizontal()
            .id_salt(carousel_id)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (image, cache_type) in images {
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
                            |ui, url, renderable_media, gifs| {
                                let texture = handle_repaint(
                                    ui,
                                    retrieve_latest_texture(&image, gifs, renderable_media),
                                );
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
                                            (image.clone(), cache_type.clone()),
                                        );
                                    });
                                }

                                copy_link(url, img_resp);
                            },
                        );
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
                        |ui, url, renderable_media, gifs| {
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
}

fn copy_link(url: &str, img_resp: Response) {
    img_resp.context_menu(|ui| {
        if ui.button("Copy Link").clicked() {
            ui.ctx().copy_text(url.to_owned());
            ui.close_menu();
        }
    });
}
