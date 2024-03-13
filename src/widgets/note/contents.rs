use crate::{colors, Damus};
use egui::{Hyperlink, Image, RichText};
use nostrdb::{BlockType, Mention, Note, NoteKey, Transaction};
use tracing::warn;

pub struct NoteContents<'a> {
    damus: &'a mut Damus,
    txn: &'a Transaction,
    note: &'a Note<'a>,
    note_key: NoteKey,
}

impl<'a> NoteContents<'a> {
    pub fn new(
        damus: &'a mut Damus,
        txn: &'a Transaction,
        note: &'a Note,
        note_key: NoteKey,
    ) -> Self {
        NoteContents {
            damus,
            txn,
            note,
            note_key,
        }
    }
}

impl egui::Widget for NoteContents<'_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        render_note_contents(ui, self.damus, self.txn, self.note, self.note_key).response
    }
}

fn render_note_contents(
    ui: &mut egui::Ui,
    damus: &mut Damus,
    txn: &Transaction,
    note: &Note,
    note_key: NoteKey,
) -> egui::InnerResponse<()> {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let mut images: Vec<String> = vec![];

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
                BlockType::MentionBech32 => {
                    ui.colored_label(colors::PURPLE, "@");
                    match block.as_mention().unwrap() {
                        Mention::Pubkey(npub) => {
                            let profile = damus.ndb.get_profile_by_pubkey(txn, npub.pubkey()).ok();
                            if let Some(name) = profile
                                .as_ref()
                                .and_then(|p| crate::profile::get_profile_name(p))
                            {
                                ui.colored_label(colors::PURPLE, name);
                            } else {
                                ui.colored_label(colors::PURPLE, "nostrich");
                            }
                        }
                        _ => {
                            ui.colored_label(colors::PURPLE, block.as_str());
                        }
                    }
                }

                BlockType::Hashtag => {
                    ui.colored_label(colors::PURPLE, "#");
                    ui.colored_label(colors::PURPLE, block.as_str());
                }

                BlockType::Url => {
                    /*
                    let url = block.as_str().to_lowercase();
                    if url.ends_with("png") || url.ends_with("jpg") {
                        images.push(url);
                    } else {
                    */
                    ui.add(Hyperlink::from_label_and_url(
                        RichText::new(block.as_str()).color(colors::PURPLE),
                        block.as_str(),
                    ));
                    //}
                }

                BlockType::Text => {
                    ui.label(block.as_str());
                }

                _ => {
                    ui.colored_label(colors::PURPLE, block.as_str());
                }
            }
        }
    });

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
