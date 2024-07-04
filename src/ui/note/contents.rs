use crate::ui::note::NoteOptions;
use crate::{colors, ui, Damus};
use egui::{Color32, Hyperlink, Image, RichText};
use nostrdb::{BlockType, Mention, Note, NoteKey, Transaction};
use tracing::warn;

pub struct NoteContents<'a> {
    damus: &'a mut Damus,
    txn: &'a Transaction,
    note: &'a Note<'a>,
    note_key: NoteKey,
    options: NoteOptions,
}

impl<'a> NoteContents<'a> {
    pub fn new(
        damus: &'a mut Damus,
        txn: &'a Transaction,
        note: &'a Note,
        note_key: NoteKey,
        options: ui::note::NoteOptions,
    ) -> Self {
        NoteContents {
            damus,
            txn,
            note,
            note_key,
            options,
        }
    }
}

impl egui::Widget for NoteContents<'_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        render_note_contents(
            ui,
            self.damus,
            self.txn,
            self.note,
            self.note_key,
            self.options,
        )
        .response
    }
}

/// Render an inline note preview with a border. These are used when
/// notes are references within a note
fn render_note_preview(
    ui: &mut egui::Ui,
    app: &mut Damus,
    txn: &Transaction,
    id: &[u8; 32],
    _id_str: &str,
) -> egui::Response {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let note = if let Ok(note) = app.ndb.get_note_by_id(txn, id) {
        // TODO: support other preview kinds
        if note.kind() == 1 {
            note
        } else {
            return ui.colored_label(
                Color32::RED,
                format!("TODO: can't preview kind {}", note.kind()),
            );
        }
    } else {
        return ui.colored_label(Color32::RED, "TODO: COULD NOT LOAD");
        /*
        return ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.colored_label(colors::PURPLE, "@");
                ui.colored_label(colors::PURPLE, &id_str[4..16]);
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
            ui::NoteView::new(app, &note)
                .actionbar(false)
                .small_pfp(true)
                .wide(true)
                .note_previews(false)
                .show(ui);
        })
        .response
}

fn render_note_contents(
    ui: &mut egui::Ui,
    damus: &mut Damus,
    txn: &Transaction,
    note: &Note,
    note_key: NoteKey,
    options: NoteOptions,
) -> egui::InnerResponse<()> {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let selectable = options.has_selectable_text();
    let mut images: Vec<String> = vec![];
    let mut inline_note: Option<(&[u8; 32], &str)> = None;

    let resp = ui.horizontal_wrapped(|ui| {
        let blocks = if let Ok(blocks) = damus.ndb.get_blocks_by_key(txn, note_key) {
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
                        ui.add(ui::Mention::new(damus, txn, profile.pubkey()));
                    }

                    Mention::Pubkey(npub) => {
                        ui.add(ui::Mention::new(damus, txn, npub.pubkey()));
                    }

                    Mention::Note(note) if options.has_note_previews() => {
                        inline_note = Some((note.id(), block.as_str()));
                    }

                    Mention::Event(note) if options.has_note_previews() => {
                        inline_note = Some((note.id(), block.as_str()));
                    }

                    _ => {
                        ui.colored_label(colors::PURPLE, format!("@{}", &block.as_str()[4..16]));
                    }
                },

                BlockType::Hashtag => {
                    #[cfg(feature = "profiling")]
                    puffin::profile_scope!("hashtag contents");
                    ui.colored_label(colors::PURPLE, format!("#{}", block.as_str()));
                }

                BlockType::Url => {
                    let lower_url = block.as_str().to_lowercase();
                    if lower_url.ends_with("png") || lower_url.ends_with("jpg") {
                        images.push(block.as_str().to_string());
                    } else {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("url contents");
                        ui.add(Hyperlink::from_label_and_url(
                            RichText::new(block.as_str()).color(colors::PURPLE),
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
                    ui.colored_label(colors::PURPLE, block.as_str());
                }
            }
        }
    });

    if let Some((id, block_str)) = inline_note {
        render_note_preview(ui, damus, txn, id, block_str);
    }

    if !images.is_empty() && !damus.textmode {
        ui.add_space(2.0);
        let carousel_id = egui::Id::new(("carousel", note.key().expect("expected tx note")));
        image_carousel(ui, images, carousel_id);
        ui.add_space(2.0);
    }

    resp
}

fn image_carousel(ui: &mut egui::Ui, images: Vec<String>, carousel_id: egui::Id) {
    // let's make sure everything is within our area

    let height = 360.0;
    let width = ui.available_size().x;

    ui.add_sized([width, height], |ui: &mut egui::Ui| {
        egui::ScrollArea::horizontal()
            .id_source(carousel_id)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for image in images {
                        let img_resp = ui.add(
                            Image::new(image.clone())
                                .max_height(height)
                                .rounding(5.0)
                                .fit_to_original_size(1.0),
                        );
                        img_resp.context_menu(|ui| {
                            if ui.button("Copy Link").clicked() {
                                ui.ctx().copy_text(image);
                                ui.close_menu();
                            }
                        });
                    }
                })
                .response
            })
            .inner
    });
}
