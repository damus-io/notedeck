use crate::ui::{
    self,
    note::{NoteOptions, NoteResponse},
};
use crate::{actionbar::NoteAction, images::ImageType, timeline::TimelineKind};
use egui::{vec2, Button, Color32, Hyperlink, Image, Rect, RichText, Vec2, Window};
use nostrdb::{BlockType, Mention, Ndb, Note, NoteKey, Transaction};
use tracing::warn;

use notedeck::{ImageCache, NoteCache};

pub struct NoteContents<'a> {
    ndb: &'a Ndb,
    img_cache: &'a mut ImageCache,
    note_cache: &'a mut NoteCache,
    txn: &'a Transaction,
    note: &'a Note<'a>,
    note_key: NoteKey,
    options: NoteOptions,
    action: Option<NoteAction>,
}

impl<'a> NoteContents<'a> {
    pub fn new(
        ndb: &'a Ndb,
        img_cache: &'a mut ImageCache,
        note_cache: &'a mut NoteCache,
        txn: &'a Transaction,
        note: &'a Note,
        note_key: NoteKey,
        options: ui::note::NoteOptions,
    ) -> Self {
        NoteContents {
            ndb,
            img_cache,
            note_cache,
            txn,
            note,
            note_key,
            options,
            action: None,
        }
    }

    pub fn action(&self) -> &Option<NoteAction> {
        &self.action
    }
}

impl egui::Widget for &mut NoteContents<'_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let result = render_note_contents(
            ui,
            self.ndb,
            self.img_cache,
            self.note_cache,
            self.txn,
            self.note,
            self.note_key,
            self.options,
        );
        self.action = result.action;
        result.response
    }
}

/// Render an inline note preview with a border. These are used when
/// notes are references within a note
pub fn render_note_preview(
    ui: &mut egui::Ui,
    ndb: &Ndb,
    note_cache: &mut NoteCache,
    img_cache: &mut ImageCache,
    txn: &Transaction,
    id: &[u8; 32],
    parent: NoteKey,
) -> NoteResponse {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let note = if let Ok(note) = ndb.get_note_by_id(txn, id) {
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

    egui::Frame::none()
        .fill(ui.visuals().noninteractive().weak_bg_fill)
        .inner_margin(egui::Margin::same(8.0))
        .outer_margin(egui::Margin::symmetric(0.0, 8.0))
        .rounding(egui::Rounding::same(10.0))
        .stroke(egui::Stroke::new(
            1.0,
            ui.visuals().noninteractive().bg_stroke.color,
        ))
        .show(ui, |ui| {
            ui::NoteView::new(ndb, note_cache, img_cache, &note)
                .actionbar(false)
                .small_pfp(true)
                .wide(true)
                .note_previews(false)
                .options_button(true)
                .parent(parent)
                .show(ui)
        })
        .inner
}

fn is_image_link(url: &str) -> bool {
    url.ends_with("png") || url.ends_with("jpg") || url.ends_with("jpeg")
}

#[allow(clippy::too_many_arguments)]
fn render_note_contents(
    ui: &mut egui::Ui,
    ndb: &Ndb,
    img_cache: &mut ImageCache,
    note_cache: &mut NoteCache,
    txn: &Transaction,
    note: &Note,
    note_key: NoteKey,
    options: NoteOptions,
) -> NoteResponse {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let selectable = options.has_selectable_text();
    let mut images: Vec<String> = vec![];
    let mut note_action: Option<NoteAction> = None;
    let mut inline_note: Option<(&[u8; 32], &str)> = None;
    let hide_media = options.has_hide_media();
    let link_color = ui.visuals().hyperlink_color;

    let response = ui.horizontal_wrapped(|ui| {
        let blocks = if let Ok(blocks) = ndb.get_blocks_by_key(txn, note_key) {
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
                        let act = ui::Mention::new(ndb, img_cache, txn, profile.pubkey())
                            .show(ui)
                            .inner;
                        if act.is_some() {
                            note_action = act;
                        }
                    }

                    Mention::Pubkey(npub) => {
                        let act = ui::Mention::new(ndb, img_cache, txn, npub.pubkey())
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
                    let lower_url = block.as_str().to_lowercase();
                    if !hide_media && is_image_link(&lower_url) {
                        images.push(block.as_str().to_string());
                    } else {
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
                    ui.add(egui::Label::new(block.as_str()).selectable(selectable));
                }

                _ => {
                    ui.colored_label(link_color, block.as_str());
                }
            }
        }
    });

    let preview_note_action = if let Some((id, _block_str)) = inline_note {
        render_note_preview(ui, ndb, note_cache, img_cache, txn, id, note_key).action
    } else {
        None
    };

    if !images.is_empty() && !options.has_textmode() {
        ui.add_space(2.0);
        let carousel_id = egui::Id::new(("carousel", note.key().expect("expected tx note")));
        image_carousel(ui, img_cache, images, carousel_id);
        ui.add_space(2.0);
    }

    let note_action = preview_note_action.or(note_action);

    NoteResponse::new(response.response).with_action(note_action)
}

fn image_carousel(
    ui: &mut egui::Ui,
    img_cache: &mut ImageCache,
    images: Vec<String>,
    carousel_id: egui::Id,
) {
    let height = 360.0;
    let width = ui.available_size().x;
    let spinsz = if height > width { width } else { height };

    for image in &images {
        // Use a specific key for thumbnails
        let thumb_key = format!("{}_thumb", image);
        if !img_cache.map().contains_key(&thumb_key) {
            tracing::info!("Loading thumbnail image: {}", image);
            let res = crate::images::fetch_img(
                img_cache,
                ui.ctx(),
                image,
                ImageType::Content(width.round() as u32, height.round() as u32),
            );
            img_cache.map_mut().insert(thumb_key.clone(), res);
        }
    }

    let show_popup = ui.ctx().memory(|mem| {
        mem.data
            .get_temp(carousel_id.with("show_popup"))
            .unwrap_or(false)
    });

    let current_image_opt = if show_popup {
        Some(ui.ctx().memory(|mem| {
            mem.data
                .get_temp::<String>(carousel_id.with("current_image"))
                .unwrap_or_else(|| images[0].clone())
        }))
    } else {
        None
    };

    ui.add_sized([width, height], |ui: &mut egui::Ui| {
        egui::ScrollArea::horizontal()
            .id_salt(carousel_id)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for image in images {
                        let thumb_key = format!("{}_thumb", image);
                        match img_cache.map().get(&thumb_key).and_then(|p| p.ready()) {
                            Some(Ok(img)) => {
                                let img_resp = ui.add(
                                    Button::image(
                                        Image::new(img)
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
                                            image.clone(),
                                        );
                                    });

                                    // Load full-res image with a distinct key
                                    let full_res_key = format!("{}_fullres", image);
                                    if !img_cache.map().contains_key(&full_res_key) {
                                        tracing::info!("Loading full resolution image: {}", image);

                                        let res = crate::images::fetch_img(
                                            img_cache,
                                            ui.ctx(),
                                            &image,
                                            ImageType::Original,
                                        );
                                        img_cache.map_mut().insert(full_res_key, res);
                                    } else {
                                        tracing::info!(
                                            "Using cached full resolution image: {}",
                                            image
                                        );
                                    }
                                }
                            }
                            _ => {
                                ui.allocate_space(egui::vec2(spinsz, spinsz));
                            }
                        }
                    }
                })
                .response
            })
            .inner
    });

    if show_popup {
        let current_image = current_image_opt.as_ref().unwrap();

        let full_res_key = format!("{}_fullres", current_image);
        let thumb_key = format!("{}_thumb", current_image);

        // Use thumbnail as fallback
        let thumbnail_img = img_cache
            .map()
            .get(&thumb_key)
            .and_then(|p| p.ready())
            .and_then(|r| r.as_ref().ok());

        // Get from cache using the fullres key
        let full_res_img = img_cache
            .map()
            .get(&full_res_key)
            .and_then(|p| p.ready())
            .and_then(|r| r.as_ref().ok())
            .or(thumbnail_img);

        if let Some(img) = full_res_img {
            Window::new("image_popup")
                .title_bar(false)
                .fixed_size(ui.ctx().screen_rect().size())
                .fixed_pos(ui.ctx().screen_rect().min)
                .frame(egui::Frame::none())
                .show(ui.ctx(), |ui| {
                    let screen_rect = ui.ctx().screen_rect();

                    // Escape key
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        ui.ctx().memory_mut(|mem| {
                            mem.data.insert_temp(carousel_id.with("show_popup"), false);
                        });
                    }

                    // background
                    let bg_response = ui.allocate_rect(screen_rect, egui::Sense::click());
                    if bg_response.clicked() {
                        ui.ctx().memory_mut(|mem| {
                            mem.data.insert_temp(carousel_id.with("show_popup"), false);
                        });
                    }
                    ui.painter()
                        .rect_filled(screen_rect, 0.0, Color32::from_black_alpha(230));

                    // Close button
                    let close_btn = ui.put(
                        Rect::from_min_size(
                            screen_rect.right_top() + vec2(-50.0, 10.0),
                            Vec2::new(40.0, 40.0),
                        ),
                        Button::new(RichText::new("✕").size(24.0).color(Color32::WHITE))
                            .frame(false),
                    );
                    if close_btn.clicked() {
                        ui.ctx().memory_mut(|mem| {
                            mem.data.insert_temp(carousel_id.with("show_popup"), false);
                        });
                    }

                    ui.centered_and_justified(|ui| {
                        let base_size = img.size_vec2();
                        let available_space = screen_rect.size() - Vec2::new(40.0, 40.0);
                        let scale_x = available_space.x / base_size.x;
                        let scale_y = available_space.y / base_size.y;
                        let scale = scale_x.min(scale_y).min(1.0);
                        let display_size = base_size * scale;

                        let image_response = ui.add(
                            Image::new(img)
                                .fit_to_exact_size(display_size)
                                .rounding(5.0),
                        );

                        // Prevent clicks on the image from closing the popup
                        if image_response.clicked() {
                            bg_response.clicked();
                        }
                    });
                });
        }
    }
}
