use enostr::{NoteId, Pubkey};
use nostrdb::Note;

#[derive(Clone)]
#[allow(clippy::enum_variant_names)]
pub enum NoteOptionSelection {
    CopyText,
    CopyPubkey,
    CopyNoteId,
}

impl NoteOptionSelection {
    pub fn process(&self, ui: &mut egui::Ui, note: &Note<'_>) {
        match self {
            NoteOptionSelection::CopyText => {
                ui.output_mut(|w| {
                    w.copied_text = note.content().to_string();
                });
            }
            NoteOptionSelection::CopyPubkey => {
                ui.output_mut(|w| {
                    if let Some(bech) = Pubkey::new(*note.pubkey()).to_bech() {
                        w.copied_text = bech;
                    }
                });
            }
            NoteOptionSelection::CopyNoteId => {
                ui.output_mut(|w| {
                    if let Some(bech) = NoteId::new(*note.id()).to_bech() {
                        w.copied_text = bech;
                    }
                });
            }
        }
    }
}
