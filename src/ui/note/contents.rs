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
        //.fill(egui::Color32::BLACK.gamma_multiply(0.2))
        //
        .rounding(egui::Rounding::same(10.0))
        .stroke(egui::Stroke::new(
            1.0,
            egui::Color32::from_rgb(0x2C, 0x2C, 0x2C),
        ))
        .show(ui, |ui| {
            ui.add(
                ui::Note::new(app, &note)
                    .actionbar(false)
                    .note_previews(false),
            )
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

    let images: Vec<String> = vec![];
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
                    Mention::Pubkey(npub) => {
                        #[cfg(feature = "profiling")]
                        puffin::profile_scope!("pubkey contents");

                        ui.horizontal(|ui| {
                            let profile = damus.ndb.get_profile_by_pubkey(txn, npub.pubkey()).ok();

                            let name: String = if let Some(name) =
                                profile.as_ref().and_then(crate::profile::get_profile_name)
                            {
                                format!("@{}", name.username())
                            } else {
                                "??".to_string()
                            };

                            let resp = ui.colored_label(colors::PURPLE, &name);

                            if let Some(rec) = profile.as_ref() {
                                resp.on_hover_ui_at_pointer(|ui| {
                                    ui.set_max_width(300.0);
                                    ui.add(ui::ProfilePreview::new(rec, &mut damus.img_cache));
                                });
                            }
                        });
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
                    /*
                    let url = block.as_str().to_lowercase();
                    if url.ends_with("png") || url.ends_with("jpg") {
                        images.push(url);
                    } else {
                    */
                    #[cfg(feature = "profiling")]
                    puffin::profile_scope!("url contents");
                    ui.add(Hyperlink::from_label_and_url(
                        RichText::new(block.as_str()).color(colors::PURPLE),
                        block.as_str(),
                    ));
                    //}
                }

                BlockType::Text => {
                    #[cfg(feature = "profiling")]
                    puffin::profile_scope!("text contents");
                    ui.label(block.as_str());
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

    for image in images {
        let img_resp = ui.add(Image::new(image.clone()));
        img_resp.context_menu(|ui| {
            if ui.button("Copy Link").clicked() {
                ui.ctx().copy_text(image);
                ui.close_menu();
            }
        });
    }

    resp
}
