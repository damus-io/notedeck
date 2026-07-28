//! The longform (NIP-23, kind 30023) note editor: a full-screen markdown editor
//! with a live rendered preview, paired with the canvas in [`crate::Notebook`].
//!
//! Like the canvas UI ([`crate::ui`]), this module is deliberately
//! persistence-free: [`editor_ui`] edits the working buffers in place and returns
//! at most one [`EditorAction`] for the frame. The app ([`crate::Notebook::render`])
//! turns that into a signed kind-30023 event via [`crate::store::create_longform`]
//! / [`crate::store::edit_longform`]. Keeping the sign/ingest out of here mirrors
//! how the canvas returns a single [`crate::UiIntent`] for the caller to apply, and
//! lets the editor be driven headlessly in tests.

use crate::event::LongformNote;
use egui::{Layout, RichText, ScrollArea, TextEdit};
use notedeck::AppContext;

/// Minimum visible rows the source editor requests before it scrolls, so a fresh
/// note still opens with a roomy typing area.
const SOURCE_MIN_ROWS: usize = 20;

/// The persisted identity of the note an editor is bound to: its stable
/// replaceable-event `d` and the `created_at` of the last version we wrote. The
/// `created_at` is the supersede baseline handed to
/// [`crate::store::edit_longform`] on the next save.
pub(crate) struct SavedLongform {
    /// Stable `d`, minted on the first save and reused by every later edit.
    pub d: String,
    /// `created_at` of the last version we persisted.
    pub created_at: u64,
}

/// State for the full-screen longform editor. The `title`/`content` buffers are
/// edited in place each frame; `saved` plus the `saved_*` snapshots record what's
/// been persisted so [`LongformEditor::dirty`] can report unsaved changes.
///
/// There is no vault/browse yet (a later card), so an editor is only ever created
/// fresh via [`LongformEditor::new`]; reopening an existing note by `d` is future
/// work once a note list exists.
pub(crate) struct LongformEditor {
    /// Working title buffer (the NIP-23 `title` tag).
    pub title: String,
    /// Working markdown body buffer (the note content).
    pub content: String,
    /// The persisted note this editor is bound to, or `None` if never saved.
    pub saved: Option<SavedLongform>,
    /// The last-persisted title, to detect an unsaved title change.
    saved_title: String,
    /// The last-persisted content, to detect an unsaved body change.
    saved_content: String,
}

impl LongformEditor {
    /// A fresh, empty editor for composing a brand-new note.
    pub(crate) fn new() -> Self {
        LongformEditor {
            title: String::new(),
            content: String::new(),
            saved: None,
            saved_title: String::new(),
            saved_content: String::new(),
        }
    }

    /// An editor bound to an existing note (opened from the vault), its buffers
    /// seeded from `note` and clean: `saved` carries the note's `d` +
    /// `created_at`, so the next save supersedes it rather than minting a new one.
    pub(crate) fn open(note: &LongformNote) -> Self {
        LongformEditor {
            title: note.title.clone(),
            content: note.content.clone(),
            saved: Some(SavedLongform {
                d: note.d.clone(),
                created_at: note.created_at,
            }),
            saved_title: note.title.clone(),
            saved_content: note.content.clone(),
        }
    }

    /// Whether the working buffers differ from what was last persisted — i.e.
    /// there are unsaved changes worth writing. A brand-new note with any text is
    /// dirty (its baseline is empty); a freshly-saved note is clean until edited.
    pub(crate) fn dirty(&self) -> bool {
        self.title != self.saved_title || self.content != self.saved_content
    }

    /// Whether the note is effectively empty (nothing worth persisting yet). Used
    /// to skip creating a blank note, mirroring the canvas's blank-node discard.
    pub(crate) fn is_blank(&self) -> bool {
        self.title.trim().is_empty() && self.content.trim().is_empty()
    }

    /// Record the buffers just persisted (as `saved`) as the new clean baseline.
    /// Called by the app after a successful save so `dirty()` reads false again.
    pub(crate) fn mark_saved(&mut self, saved: SavedLongform) {
        self.saved_title = self.title.clone();
        self.saved_content = self.content.clone();
        self.saved = Some(saved);
    }
}

/// The single action the editor surfaces per frame for the app to act on. Save
/// and Close are mutually exclusive within a frame.
pub(crate) enum EditorAction {
    /// Persist the working buffers (create if new, else supersede the saved note).
    Save,
    /// Leave the editor and return to the canvas.
    Close,
}

/// Render the full-screen longform editor: a header (Close / title / Save) over a
/// split of the markdown source and its live rendered preview. Edits land in
/// `editor`'s buffers directly; the frame's single [`EditorAction`] (if any) is
/// returned for [`crate::Notebook::render`] to persist or dismiss.
pub(crate) fn editor_ui(
    editor: &mut LongformEditor,
    ctx: &mut AppContext,
    ui: &mut egui::Ui,
) -> Option<EditorAction> {
    let mut action = None;

    // Header: a back button on the left; the title field, a dirty marker and Save
    // on the right (laid out right-to-left so the title fills the gap between).
    ui.horizontal(|ui| {
        if ui.button("← Canvas").clicked() {
            action = Some(EditorAction::Close);
        }
        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
            let dirty = editor.dirty();
            if ui.add_enabled(dirty, egui::Button::new("Save")).clicked() {
                action = Some(EditorAction::Save);
            }
            if dirty {
                ui.label(RichText::new("●").weak())
                    .on_hover_text("Unsaved changes");
            }
            // The title takes whatever horizontal room is left in the row.
            ui.add(
                TextEdit::singleline(&mut editor.title)
                    .hint_text("Untitled")
                    .desired_width(f32::INFINITY),
            );
        });
    });

    ui.separator();

    body_ui(editor, ctx, ui);

    action
}

/// The editor body: source on the left, preview on the right (side-by-side on a
/// wide viewport; stacked in a single scroll column when narrow).
fn body_ui(editor: &mut LongformEditor, ctx: &mut AppContext, ui: &mut egui::Ui) {
    let height = ui.available_height();

    if notedeck::ui::is_narrow(ui.ctx()) {
        // Narrow: one scroll column — the source editor, then the live preview
        // beneath it. The editor keeps a fixed minimum so both stay usable.
        ScrollArea::vertical()
            .id_salt("notebook-editor-narrow")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add(
                    TextEdit::multiline(&mut editor.content)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows(12),
                );
                ui.add_space(notedeck::tokens::SPACING_MD);
                ui.separator();
                ui.add_space(notedeck::tokens::SPACING_MD);
                preview_body_ui(ui, ctx, &editor.content);
            });
        return;
    }

    // Wide: source | preview, each its own vertically-scrolling column.
    ui.columns(2, |cols| {
        source_column_ui(&mut cols[0], &mut editor.content, height);
        preview_column_ui(&mut cols[1], ctx, &editor.content, height);
    });
}

/// The left column: a monospace markdown source editor that fills the column and
/// scrolls when the note outgrows it.
fn source_column_ui(ui: &mut egui::Ui, content: &mut String, height: f32) {
    ScrollArea::vertical()
        .id_salt("notebook-editor-source")
        .max_height(height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add(
                TextEdit::multiline(content)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .desired_rows(SOURCE_MIN_ROWS),
            );
        });
}

/// The right column: the rendered markdown preview, scrolling independently of
/// the source.
fn preview_column_ui(ui: &mut egui::Ui, ctx: &mut AppContext, content: &str, height: f32) {
    ScrollArea::vertical()
        .id_salt("notebook-editor-preview")
        .max_height(height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            preview_body_ui(ui, ctx, content);
        });
}

/// Render `content` as the live preview, resolving any inline `nostr:` references
/// exactly like a canvas text node (see
/// [`notedeck_ui::markdown::render_markdown_with_refs`]). Shows a placeholder
/// while the note is empty so the pane never looks broken.
fn preview_body_ui(ui: &mut egui::Ui, ctx: &mut AppContext, content: &str) {
    if content.trim().is_empty() {
        ui.weak("Nothing to preview yet.");
        return;
    }
    notedeck_ui::markdown::render_markdown_with_refs(ui, ctx, content);
}

/// Render the vault list — one selectable row per note (newest-edited first) —
/// and return the index of a note clicked this frame, for the caller to open.
/// Reads a borrowed slice and builds no owned collections, so it's safe to call
/// every frame from the canvas-mode sidebar (the list itself is cached upstream).
pub(crate) fn vault_ui(notes: &[LongformNote], ui: &mut egui::Ui) -> Option<usize> {
    ui.add_space(notedeck::tokens::SPACING_SM);
    ui.strong("Notes");
    ui.separator();

    if notes.is_empty() {
        ui.weak("No notes yet.");
        return None;
    }

    let mut open = None;
    ScrollArea::vertical()
        .id_salt("notebook-vault-list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, note) in notes.iter().enumerate() {
                let title = note.title.trim();
                let label = if title.is_empty() { "Untitled" } else { title };
                if ui.selectable_label(false, label).clicked() {
                    open = Some(i);
                }
            }
        });
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dirty/baseline bookkeeping that decides when Save is offered: a new
    /// note is clean until typed into, dirty once it has text, and clean again
    /// after `mark_saved` records that text as the baseline — then dirty once more
    /// on the next edit.
    #[test]
    fn dirty_tracks_baseline_across_saves() {
        let mut editor = LongformEditor::new();
        assert!(!editor.dirty(), "a blank new editor has nothing to save");
        assert!(editor.is_blank());

        editor.content = "hello".to_string();
        assert!(editor.dirty(), "typed content is unsaved");
        assert!(!editor.is_blank());

        editor.mark_saved(SavedLongform {
            d: "abcd".to_string(),
            created_at: 100,
        });
        assert!(!editor.dirty(), "just-saved content is the clean baseline");
        assert_eq!(editor.saved.as_ref().map(|s| s.created_at), Some(100));

        editor.content = "hello world".to_string();
        assert!(editor.dirty(), "editing after a save is unsaved again");

        // A title-only change is also dirtying.
        editor.mark_saved(SavedLongform {
            d: "abcd".to_string(),
            created_at: 101,
        });
        assert!(!editor.dirty());
        editor.title = "Titled".to_string();
        assert!(editor.dirty(), "a title change alone is unsaved");
    }
}
