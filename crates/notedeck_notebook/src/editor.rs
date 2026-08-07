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
use notedeck::{AppContext, ColorTheme, Localization};
use notedeck_ui::context_menu::{PasteBehavior, input_context};

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
/// An editor is created either fresh via [`LongformEditor::new`] (the toolbar's
/// New-note button) or bound to an existing note via [`LongformEditor::open`]
/// (clicking a vault row).
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
    use notedeck::tokens::{SPACING_LG, SPACING_SM};
    let mut action = None;
    let theme = notedeck::ColorTheme::current(ui.ctx());

    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(SPACING_LG as i8, SPACING_SM as i8))
        .show(ui, |ui| {
            // Header: a back button on the left; the title (a borderless heading),
            // a dirty marker and Save on the right (right-to-left so the title
            // fills the gap between).
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
                        ui.label(RichText::new("●").color(theme.accent))
                            .on_hover_text("Unsaved changes");
                    }
                    ui.add(
                        TextEdit::singleline(&mut editor.title)
                            .hint_text("Untitled")
                            .font(egui::TextStyle::Heading)
                            .frame(false)
                            .desired_width(f32::INFINITY),
                    );
                });
            });

            ui.add_space(SPACING_SM);
            ui.separator();
            ui.add_space(SPACING_SM);

            body_ui(editor, ctx, ui, &theme);
        });

    action
}

/// The editor body: source on the left, preview on the right (side-by-side on a
/// wide viewport; stacked in a single scroll column when narrow).
fn body_ui(
    editor: &mut LongformEditor,
    ctx: &mut AppContext,
    ui: &mut egui::Ui,
    theme: &ColorTheme,
) {
    let height = ui.available_height();

    if notedeck::ui::is_narrow(ui.ctx()) {
        // Narrow: one scroll column — the source editor panel, then the live
        // preview beneath it.
        ScrollArea::vertical()
            .id_salt("notebook-editor-narrow")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                source_panel_ui(ui, &mut editor.content, ctx, theme, SOURCE_MIN_ROWS);
                ui.add_space(notedeck::tokens::SPACING_MD);
                preview_body_ui(ui, ctx, &editor.content);
            });
        return;
    }

    // Wide: source | preview, each its own vertically-scrolling column.
    ui.columns(2, |cols| {
        source_column_ui(&mut cols[0], &mut editor.content, ctx, height, theme);
        preview_column_ui(&mut cols[1], ctx, &editor.content, height);
    });
}

/// The left column: a monospace markdown source editor in a subtle rounded panel
/// that fills the column height and scrolls when the note outgrows it.
fn source_column_ui(
    ui: &mut egui::Ui,
    content: &mut String,
    ctx: &mut AppContext,
    height: f32,
    theme: &ColorTheme,
) {
    let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
    let rows = ((height / row_h) as usize).max(SOURCE_MIN_ROWS);
    ScrollArea::vertical()
        .id_salt("notebook-editor-source")
        .max_height(height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            source_panel_ui(ui, content, ctx, theme, rows);
        });
}

/// The monospace source editor itself: a frameless code editor inside a rounded
/// `surface_secondary` panel, so the raw markdown reads as a distinct "source"
/// pane against the rendered preview. A right-click Paste/Copy/Cut menu (plus
/// middle-click paste) is attached via the shared
/// [`input_context`](notedeck_ui::context_menu::input_context) helper — the same
/// one the rest of notedeck's text inputs use — so editing the note's text works
/// the same everywhere. It's selection-aware: Copy/Cut act on the highlighted
/// span and Paste ([`PasteBehavior::Append`]) drops the clipboard over the
/// selection or at the caret (never clearing the whole note).
fn source_panel_ui(
    ui: &mut egui::Ui,
    content: &mut String,
    ctx: &mut AppContext,
    theme: &ColorTheme,
    rows: usize,
) {
    egui::Frame::new()
        .fill(theme.surface_secondary)
        .corner_radius(egui::CornerRadius::same(notedeck::tokens::RADIUS_MD as u8))
        .inner_margin(egui::Margin::same(notedeck::tokens::SPACING_SM as i8))
        .show(ui, |ui| {
            let resp = ui.add(
                TextEdit::multiline(content)
                    .code_editor()
                    .frame(false)
                    .desired_width(f32::INFINITY)
                    .desired_rows(rows),
            );
            input_context(ui, &resp, ctx.clipboard, content, PasteBehavior::Append);
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
    // The preview holds no transaction; open one here and pass it in so the
    // markdown's inline references resolve.
    let mut note_ctx = ctx.note_context();
    let txn = nostrdb::Transaction::new(note_ctx.ndb).expect("preview txn");
    notedeck_ui::markdown::render_markdown_with_refs(ui, &mut note_ctx, &txn, content);
}

/// A vault row ready to render: a note's display strings, precomputed once when
/// the vault list is rebuilt (see [`vault_rows`]) rather than per frame in
/// [`vault_ui`] — the render loop must not allocate or format (CLAUDE.md rule 18).
/// The full [`LongformNote`] stays in the sync cache; the app resolves it by
/// [`d`](Self::d) only when a row is actually opened/renamed/deleted.
pub(crate) struct VaultRow {
    /// The note's stable addressable `d`, the identity carried in every
    /// [`VaultAction`] so the app can look the full note back up.
    pub d: String,
    /// The note title (already trimmed of surrounding whitespace, but possibly
    /// empty — the row renders "Untitled" for a blank title).
    pub title: String,
    /// The muted "edited …" subtitle, e.g. "edited 2h ago" (empty if unavailable).
    pub subtitle: String,
}

/// Project the sync cache's notes into renderable [`VaultRow`]s, formatting each
/// row's relative "edited …" subtitle once. Called on the vault relist path (not
/// per frame), so the localized time string and its allocation stay out of the
/// render loop.
pub(crate) fn vault_rows(i18n: &mut Localization, notes: &[LongformNote]) -> Vec<VaultRow> {
    let now = notedeck::unix_time_secs();
    notes
        .iter()
        .map(|note| VaultRow {
            d: note.d.clone(),
            title: note.title.trim().to_owned(),
            subtitle: edited_subtitle(i18n, note.created_at, now),
        })
        .collect()
}

/// The muted "edited …" subtitle for a vault row, relative to `now`. A note's
/// `created_at` is its last-edited time — kind 30023 is replaceable, so each edit
/// republishes with a fresh timestamp — so this reads as when the note was last
/// touched: "edited just now" for a very recent edit, else "edited <relative> ago"
/// (e.g. "edited 2h ago"), reusing notedeck's shared relative-time bucketer.
fn edited_subtitle(i18n: &mut Localization, created_at: u64, now: u64) -> String {
    // A very recent edit reads oddly through the "edited … ago" frame ("edited now
    // ago"), so phrase it directly. The same `now` drives the relative string
    // below, keeping the two consistent.
    if now.saturating_sub(created_at) <= 2 {
        return "edited just now".to_owned();
    }
    format!(
        "edited {} ago",
        notedeck::time_ago_between(i18n, created_at, now)
    )
}

/// The vault sidebar's transient interaction state — the one thing that must
/// persist across frames in this otherwise-immediate list: an in-progress rename
/// buffer, or a delete awaiting confirmation. Held in [`crate::Notebook`] and
/// driven entirely by [`vault_ui`]; every *completed* interaction leaves as a
/// [`VaultAction`] for the app to persist, so this stays purely UI-local.
#[derive(Default)]
pub(crate) enum VaultState {
    /// Nothing in progress — all rows are plain and clickable.
    #[default]
    Idle,
    /// A row is being inline-renamed.
    Renaming(VaultRename),
    /// A row's delete is awaiting modal confirmation; holds the note's `d`.
    ConfirmingDelete(String),
}

/// An in-progress inline rename of a vault row. While the vault is in
/// [`VaultState::Renaming`], the row whose note matches [`d`](Self::d) renders an
/// editable title field in place of its label; every other row stays plain.
pub(crate) struct VaultRename {
    /// The `d` of the note being renamed (its stable addressable id).
    pub d: String,
    /// Editable title buffer, seeded from the note's current title.
    pub buffer: String,
    /// True only until the field has grabbed keyboard focus (its first frame).
    pub focus: bool,
}

/// A completed vault interaction the app persists this frame — at most one,
/// mirroring the canvas's single [`crate::UiIntent`]. In-progress steps (arming a
/// rename, opening the delete prompt) mutate the passed-in [`VaultState`] and
/// yield nothing; only a terminal action surfaces here.
pub(crate) enum VaultAction {
    /// Open note `d` in the editor.
    Open { d: String },
    /// A rename committed: persist `title` as note `d`'s new title.
    Rename { d: String, title: String },
    /// A delete was confirmed: tombstone note `d`.
    Delete { d: String },
}

/// The outcome of one frame of an inline rename field.
#[derive(Clone, Copy)]
enum RenameOutcome {
    /// Still editing.
    Pending,
    /// Committed (Enter or click-away): persist the buffer.
    Commit,
    /// Dismissed (Esc): discard the buffer, keep the old title.
    Cancel,
}

/// Render the vault list — one styled row per note (newest-edited first) — plus
/// any in-progress rename field or delete-confirmation modal, and return the
/// single terminal [`VaultAction`] the user triggered this frame. All transient
/// interaction lives in `state`; the rows are pre-projected ([`vault_rows`]), so
/// the function reads a borrowed slice and only allocates on a discrete user
/// action — safe to call every frame from the canvas-mode sidebar.
pub(crate) fn vault_ui(
    rows: &[VaultRow],
    state: &mut VaultState,
    ui: &mut egui::Ui,
) -> Option<VaultAction> {
    use notedeck::tokens::{SPACING_SM, SPACING_XS};
    let theme = notedeck::ColorTheme::current(ui.ctx());

    // A muted, small-caps section header, left-aligned with the row titles below.
    ui.add_space(SPACING_SM);
    ui.horizontal(|ui| {
        ui.add_space(SPACING_SM);
        ui.label(
            egui::RichText::new("NOTES")
                .small()
                .strong()
                .color(theme.text_muted),
        );
    });
    ui.add_space(SPACING_XS);

    if rows.is_empty() {
        ui.add_space(SPACING_SM);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("No notes yet").color(theme.text_muted));
        });
        return None;
    }

    let mut action = None;
    ScrollArea::vertical()
        .id_salt("notebook-vault-list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for row in rows {
                // The row being renamed shows an editable field; the rest render
                // as plain, clickable rows with a context menu.
                let renaming = matches!(state, VaultState::Renaming(r) if r.d == row.d);
                let out = if renaming {
                    renaming_row_ui(ui, &theme, state)
                } else {
                    note_menu_row_ui(ui, &theme, row, state)
                };
                if out.is_some() {
                    action = out;
                }
            }
        });

    // A pending delete draws its confirmation modal over the list.
    action.or_else(|| confirm_delete_ui(ui, rows, state))
}

/// Render the active rename row and resolve its outcome: a commit clears `state`
/// to [`VaultState::Idle`] and yields a [`VaultAction::Rename`] (dropping an empty
/// title as a cancel), a cancel just clears it, still-editing leaves it armed.
/// `state` is [`VaultState::Renaming`] for this row (the caller checks).
fn renaming_row_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    state: &mut VaultState,
) -> Option<VaultAction> {
    let VaultState::Renaming(active) = state else {
        return None;
    };
    let outcome = rename_row_ui(ui, theme, active);
    let committed = matches!(outcome, RenameOutcome::Commit)
        .then(|| (active.d.clone(), active.buffer.trim().to_owned()))
        .filter(|(_, title)| !title.is_empty());
    if !matches!(outcome, RenameOutcome::Pending) {
        *state = VaultState::Idle;
    }
    committed.map(|(d, title)| VaultAction::Rename { d, title })
}

/// Render a plain, clickable vault row plus its Rename/Delete context menu.
/// Clicking opens the note; Rename arms the inline field for the next frame;
/// Delete opens the confirmation modal. Both menu entries transition `state`;
/// only a click (Open) returns an action here.
fn note_menu_row_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    row: &VaultRow,
    state: &mut VaultState,
) -> Option<VaultAction> {
    let label = if row.title.is_empty() {
        "Untitled"
    } else {
        &row.title
    };
    let resp = note_row_ui(ui, theme, label, &row.subtitle);
    let action = resp
        .clicked()
        .then(|| VaultAction::Open { d: row.d.clone() });
    // Right-click (or long-press on touch) mirrors the canvas node's menu.
    notedeck_ui::context_menu::context_menu(&resp, |ui| {
        if ui.button("Rename").clicked() {
            *state = VaultState::Renaming(VaultRename {
                d: row.d.clone(),
                buffer: row.title.clone(),
                focus: true,
            });
            ui.close_menu();
        }
        if ui.button("Delete").clicked() {
            *state = VaultState::ConfirmingDelete(row.d.clone());
            ui.close_menu();
        }
    });
    action
}

/// While a delete is pending, draw the confirmation modal and resolve it: confirm
/// clears `state` and yields a [`VaultAction::Delete`]; cancel (button, backdrop,
/// or Esc) just clears it. No-op unless `state` is [`VaultState::ConfirmingDelete`].
fn confirm_delete_ui(
    ui: &mut egui::Ui,
    rows: &[VaultRow],
    state: &mut VaultState,
) -> Option<VaultAction> {
    let VaultState::ConfirmingDelete(d) = state else {
        return None;
    };
    // The note may have vanished (a concurrent sync) — abandon the prompt if so.
    let Some(row) = rows.iter().find(|r| &r.d == d) else {
        *state = VaultState::Idle;
        return None;
    };
    match note_delete_confirm_ui(ui, &row.title) {
        DeleteConfirm::Pending => None,
        DeleteConfirm::Cancelled => {
            *state = VaultState::Idle;
            None
        }
        DeleteConfirm::Confirmed => {
            let d = d.clone();
            *state = VaultState::Idle;
            Some(VaultAction::Delete { d })
        }
    }
}

/// One vault row in rename mode: a full-width singleline title field that grabs
/// focus on its first frame. Enter or clicking away commits; Esc discards. Sized
/// to match [`note_row_ui`] so the list doesn't jump as a row flips to editing.
fn rename_row_ui(ui: &mut egui::Ui, theme: &ColorTheme, rename: &mut VaultRename) -> RenameOutcome {
    use notedeck::tokens::SPACING_SM;
    let resp = egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(SPACING_SM as i8, 2))
        .show(ui, |ui| {
            ui.add(
                TextEdit::singleline(&mut rename.buffer)
                    .desired_width(f32::INFINITY)
                    .text_color(theme.text_primary)
                    .hint_text("Untitled"),
            )
        })
        .inner;

    if std::mem::take(&mut rename.focus) {
        resp.request_focus();
    }

    if resp.lost_focus() {
        // Esc leaves the buffer behind (cancel); Enter or a click elsewhere keeps
        // the typed title (commit), matching the canvas node editor's blur-commit.
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            return RenameOutcome::Cancel;
        }
        return RenameOutcome::Commit;
    }
    RenameOutcome::Pending
}

/// The user's choice this frame from the vault delete-confirmation modal.
enum DeleteConfirm {
    /// Still open; no choice yet.
    Pending,
    /// Confirmed the deletion.
    Confirmed,
    /// Dismissed (Cancel button, backdrop click, or Esc).
    Cancelled,
}

/// A centered modal confirming deletion of the note titled `title`, returning the
/// user's choice this frame. Mirrors the canvas node's delete prompt
/// ([`crate::ui`]); a backdrop click or Esc counts as cancelling.
fn note_delete_confirm_ui(ui: &egui::Ui, title: &str) -> DeleteConfirm {
    use notedeck::tokens::{SPACING_LG, SPACING_SM};
    let shown = title.trim();
    let shown = if shown.is_empty() { "Untitled" } else { shown };
    let modal =
        egui::Modal::new(egui::Id::new("notebook_note_delete_confirm")).show(ui.ctx(), |ui| {
            ui.set_max_width(320.0);
            ui.heading("Delete note?");
            ui.add_space(SPACING_SM);
            ui.label(format!("“{shown}” will be removed from your notes."));
            ui.add_space(SPACING_LG);
            ui.horizontal(|ui| {
                let delete = egui::Button::new(
                    egui::RichText::new("Delete").color(egui::Color32::from_rgb(0xE0, 0x31, 0x31)),
                );
                if ui.add(delete).clicked() {
                    return DeleteConfirm::Confirmed;
                }
                if ui.button("Cancel").clicked() {
                    return DeleteConfirm::Cancelled;
                }
                DeleteConfirm::Pending
            })
            .inner
        });

    // The buttons take precedence; a backdrop/Esc dismissal otherwise cancels.
    match modal.inner {
        DeleteConfirm::Confirmed => DeleteConfirm::Confirmed,
        DeleteConfirm::Cancelled => DeleteConfirm::Cancelled,
        DeleteConfirm::Pending if modal.should_close() => DeleteConfirm::Cancelled,
        DeleteConfirm::Pending => DeleteConfirm::Pending,
    }
}

/// One vault row: a full-width, left-aligned, rounded surface that highlights on
/// hover, with a leading document icon, the title on top and a muted `subtitle`
/// beneath (omitted when empty). Both lines elide to a single line. Painted with
/// the semantic theme so it reads as part of the app rather than a bare label.
fn note_row_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    title: &str,
    subtitle: &str,
) -> egui::Response {
    use notedeck::tokens::{ICON_SM, RADIUS_MD, SPACING_SM};
    let pad = egui::vec2(SPACING_SM, 6.0);
    let title_h = ui.text_style_height(&egui::TextStyle::Body);
    // The subtitle line adds its own height plus a hair of leading; a blank
    // subtitle collapses the row back to a single line.
    let subtitle_h = if subtitle.is_empty() {
        0.0
    } else {
        ui.text_style_height(&egui::TextStyle::Small) + 2.0
    };
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), title_h + subtitle_h + pad.y * 2.0),
        egui::Sense::click(),
    );

    if resp.hovered() {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(RADIUS_MD as u8),
            theme.interactive_hover,
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // A fixed leading gutter holds a document glyph aligned to the title line, so
    // titles stay aligned regardless of whether a row wraps to two lines.
    let inner = rect.shrink2(pad);
    let icon_center = egui::pos2(inner.left() + ICON_SM / 2.0, inner.top() + title_h / 2.0);
    document_icon(ui.painter(), icon_center, ICON_SM, theme.text_muted);
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(inner.left() + ICON_SM + SPACING_SM, inner.top()),
        inner.max,
    );

    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(text_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.add(
                egui::Label::new(egui::RichText::new(title).color(theme.text_primary))
                    .truncate()
                    .selectable(false),
            );
            if !subtitle.is_empty() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(subtitle)
                            .small()
                            .color(theme.text_muted),
                    )
                    .truncate()
                    .selectable(false),
                );
            }
        },
    );

    resp
}

/// A small monochrome document glyph — a rounded page with three ruled text lines
/// — for a vault row's leading gutter. Painter-drawn rather than an image asset so
/// it inherits the row's muted theme color and scales with `size`. Also the
/// leading icon of a longform reference chip (see [`crate::render`]).
pub(crate) fn document_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    color: egui::Color32,
) {
    let stroke = egui::Stroke::new((size * 0.08).max(1.0), color);
    let page = egui::Rect::from_center_size(center, egui::vec2(size * 0.68, size * 0.86));
    painter.rect_stroke(
        page,
        egui::CornerRadius::same((size * 0.12) as u8),
        stroke,
        egui::StrokeKind::Inside,
    );
    // Three ruled "text" lines, inset from the page edges.
    let x0 = page.left() + size * 0.16;
    let x1 = page.right() - size * 0.16;
    for i in -1..=1 {
        let y = center.y + i as f32 * size * 0.2;
        painter.line_segment([egui::pos2(x0, y), egui::pos2(x1, y)], stroke);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The "edited …" subtitle formatting: very-recent edits read "just now"
    /// (never "now ago"), older edits frame the relative unit, and both use the
    /// same injected `now` so the phrasing is deterministic.
    #[test]
    fn edited_subtitle_phrasing() {
        let mut i18n = Localization::no_bidi();
        let now = 2_000_000u64;

        assert_eq!(edited_subtitle(&mut i18n, now, now), "edited just now");
        assert_eq!(edited_subtitle(&mut i18n, now - 2, now), "edited just now");
        assert_eq!(edited_subtitle(&mut i18n, now - 7200, now), "edited 2h ago");
        assert_eq!(
            edited_subtitle(&mut i18n, now - 86_400, now),
            "edited 1d ago"
        );
        // A note stamped slightly in the future (clock skew) still reads cleanly.
        assert_eq!(edited_subtitle(&mut i18n, now + 1, now), "edited just now");
    }

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
