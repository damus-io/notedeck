//! Inline renderer for plain nostr notes (kind 1), contributed by the columns
//! app to notedeck's [`KindRenderer`] registry so surfaces like the notebook can
//! draw a referenced note using the same `NoteView` widget the timeline uses.

use nostrdb::{Note, Transaction};
use notedeck::{KindRenderer, NoteContext};
use notedeck_ui::{NoteOptions, NoteView};

/// Renders a kind-1 nostr note inline via notedeck_ui's [`NoteView`], in a
/// compact preview style (framed, small pfp, no action bar). Registered into
/// [`notedeck::KindRendererRegistry`] at app startup.
pub struct NoteKindRenderer;

impl KindRenderer for NoteKindRenderer {
    fn id(&self) -> &'static str {
        "columns.note"
    }

    fn name(&self) -> &'static str {
        "Note"
    }

    fn kinds(&self) -> &'static [u32] {
        &[1]
    }

    fn render(
        &self,
        ui: &mut egui::Ui,
        note_context: &mut NoteContext,
        _txn: &Transaction,
        note: &Note,
    ) -> egui::Response {
        NoteView::new(note_context, note, NoteOptions::default())
            .preview_style()
            .show(ui)
            .response
    }
}
