use enostr::{NoteId, Pubkey};
use nostrdb::Note;

#[derive(Clone)]
#[allow(clippy::enum_variant_names)]
pub enum NoteOptionSelection {
    CopyText,
    CopyPubkey,
    CopyNoteId,
}

pub fn process_note_selection(
    ui: &mut egui::Ui,
    selection: Option<NoteOptionSelection>,
    note: &Note<'_>,
) {
    if let Some(option) = selection {
        match option {
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
