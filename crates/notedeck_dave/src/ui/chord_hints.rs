//! The which-key strip: while normal mode is on, a row of keycaps above the
//! chat input shows what it will accept next, with the session keys on a
//! second row at its root.

use super::keybind_hint::KeybindHint;
use super::keybindings::{ChordHint, ChordView, KeyAction};
use egui::{Align, Layout};
use notedeck::{tr, Localization};

/// Height of a keycap in the strip.
const KEYCAP: f32 = 18.0;

/// Extra keycap width per character past the first, so `za` and `gg` fit.
const KEYCAP_PER_CHAR: f32 = 8.0;

/// Height the strip reserves in the bottom-up input stack: a row of keycaps,
/// plus a second for the session keys while normal mode's root offers them.
pub fn strip_height(ui: &egui::Ui, chord: ChordView) -> f32 {
    let rows = if chord.session_keys().is_empty() {
        1.0
    } else {
        2.0
    };
    rows * KEYCAP + (rows - 1.0) * ui.spacing().item_spacing.y + notedeck::tokens::SPACING_XS
}

/// Draw the keycaps `chord` accepts, grouped and left-aligned: the pane's own
/// keys, then the session keys on a row beneath them. Keys that do nothing
/// this frame (`h` with no session list) are left out.
///
/// Runs every frame normal mode is on, so it only walks the static hint
/// tables; the one allocation, a keycap's tooltip, happens on hover.
pub fn chord_hints_ui(ui: &mut egui::Ui, i18n: &mut Localization, chord: ChordView) {
    ui.vertical(|ui| {
        hint_row_ui(ui, i18n, chord, chord.hints());
        let session_keys = chord.session_keys();
        if !session_keys.is_empty() {
            hint_row_ui(ui, i18n, chord, session_keys);
        }
    });
}

/// One row of the strip: `groups`' offered keycaps, the groups spaced apart.
fn hint_row_ui(
    ui: &mut egui::Ui,
    i18n: &mut Localization,
    chord: ChordView,
    groups: &'static [&'static [ChordHint]],
) {
    let size = egui::vec2(ui.available_width(), KEYCAP);
    ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
        let mut first_group = true;
        for group in groups {
            if !group.iter().any(|hint| chord.offers(&hint.action)) {
                continue;
            }
            if !std::mem::take(&mut first_group) {
                ui.add_space(notedeck::tokens::SPACING_MD);
            }
            for hint in group.iter().filter(|hint| chord.offers(&hint.action)) {
                let extra_chars = hint.keys.chars().count().saturating_sub(1) as f32;
                KeybindHint::new(hint.keys)
                    .size(KEYCAP)
                    .width(KEYCAP + extra_chars * KEYCAP_PER_CHAR)
                    .show(ui)
                    .on_hover_ui(|ui| {
                        if let Some(text) = describe(i18n, &hint.action) {
                            ui.label(text);
                        }
                    });
            }
        }
    });
}

/// What a chord command does, for its keycap's tooltip.
fn describe(i18n: &mut Localization, action: &KeyAction) -> Option<String> {
    Some(match action {
        KeyAction::BlockCursorDown => tr!(
            i18n,
            "Next block",
            "Dave which-key tooltip: move the block cursor down"
        ),
        KeyAction::BlockCursorUp => tr!(
            i18n,
            "Previous block",
            "Dave which-key tooltip: move the block cursor up"
        ),
        KeyAction::BlockCursorFirst => tr!(
            i18n,
            "First block",
            "Dave which-key tooltip: move the block cursor to the first block"
        ),
        KeyAction::BlockCursorLast => tr!(
            i18n,
            "Last block",
            "Dave which-key tooltip: move the block cursor to the last block"
        ),
        KeyAction::BlockToggle => tr!(
            i18n,
            "Toggle block",
            "Dave which-key tooltip: expand or collapse the focused block"
        ),
        KeyAction::BlockOpen => tr!(
            i18n,
            "Expand block",
            "Dave which-key tooltip: expand the focused block"
        ),
        KeyAction::BlockClose => tr!(
            i18n,
            "Collapse block",
            "Dave which-key tooltip: collapse the focused block"
        ),
        KeyAction::BlockExpandAll => tr!(
            i18n,
            "Expand all blocks",
            "Dave which-key tooltip: expand every block in the chat"
        ),
        KeyAction::BlockCollapseAll => tr!(
            i18n,
            "Collapse all blocks",
            "Dave which-key tooltip: collapse every block in the chat"
        ),
        KeyAction::BlockCursorClear => tr!(
            i18n,
            "Leave block navigation",
            "Dave which-key tooltip: drop the block cursor and end the chord"
        ),
        KeyAction::InsertMode => tr!(
            i18n,
            "Insert: type a message",
            "Dave which-key tooltip: leave normal mode and focus the chat input"
        ),
        KeyAction::FocusSessionsPane => tr!(
            i18n,
            "Session list",
            "Dave which-key tooltip: point the chord's motions at the session list"
        ),
        KeyAction::FocusChatPane => tr!(
            i18n,
            "Back to the chat",
            "Dave which-key tooltip: point the chord's motions back at the chat"
        ),
        KeyAction::SessionPaneNext => tr!(
            i18n,
            "Next session",
            "Dave which-key tooltip: switch to the next session in the list"
        ),
        KeyAction::SessionPanePrev => tr!(
            i18n,
            "Previous session",
            "Dave which-key tooltip: switch to the previous session in the list"
        ),
        KeyAction::SessionPaneFirst => tr!(
            i18n,
            "First session",
            "Dave which-key tooltip: switch to the first session in the list"
        ),
        KeyAction::SessionPaneLast => tr!(
            i18n,
            "Last session",
            "Dave which-key tooltip: switch to the last session in the list"
        ),
        KeyAction::NewAgent => tr!(
            i18n,
            "New agent",
            "Dave which-key tooltip: start a new agent session"
        ),
        KeyAction::CloneAgent => tr!(
            i18n,
            "Clone agent",
            "Dave which-key tooltip: start a new agent in the active session's directory"
        ),
        KeyAction::RenameAgent => tr!(
            i18n,
            "Rename session",
            "Dave which-key tooltip: rename the active session"
        ),
        KeyAction::DeleteActiveSession => tr!(
            i18n,
            "Delete session",
            "Dave which-key tooltip: delete the active session"
        ),
        KeyAction::ToggleView => tr!(
            i18n,
            "Toggle scene view",
            "Dave which-key tooltip: switch between the scene view and the list view"
        ),
        KeyAction::CyclePermissionMode => tr!(
            i18n,
            "Cycle permission mode",
            "Dave which-key tooltip: cycle the active session's permission mode"
        ),
        KeyAction::OpenExternalEditor => tr!(
            i18n,
            "Compose in external editor",
            "Dave which-key tooltip: open an external editor to write the message"
        ),
        KeyAction::FocusQueueNext => tr!(
            i18n,
            "Next in focus queue",
            "Dave which-key tooltip: jump to the next session waiting for attention"
        ),
        KeyAction::FocusQueuePrev => tr!(
            i18n,
            "Previous in focus queue",
            "Dave which-key tooltip: jump to the previous session waiting for attention"
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::super::keybindings::{Pane, Pending};
    use super::*;
    use egui::Vec2;
    use egui_kittest::Harness;

    /// The chord states' strips, one per row: `<leader>`, `z`, `g` and `d` in
    /// the chat, then `<leader>` and `g` in the session list, then `<leader>`
    /// in a narrow chat session (no `h`, no agentic keys).
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_chord_hint_strips() {
        let view = |pane, pending| ChordView {
            pending,
            pane,
            sessions_shown: true,
            agentic: true,
            interruptible: true,
        };
        let strips = [
            view(Pane::Chat, Pending::Leader),
            view(Pane::Chat, Pending::LeaderZ),
            view(Pane::Chat, Pending::LeaderG),
            view(Pane::Chat, Pending::LeaderD),
            view(Pane::Sessions, Pending::Leader),
            view(Pane::Sessions, Pending::LeaderG),
            ChordView {
                sessions_shown: false,
                agentic: false,
                ..view(Pane::Chat, Pending::Leader)
            },
        ];
        let mut harness = Harness::builder()
            .with_size(Vec2::new(480.0, 300.0))
            .renderer(notedeck::software_renderer())
            .build_ui_state(
                |ui, i18n: &mut Localization| {
                    for chord in strips {
                        let height = strip_height(ui, chord);
                        ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
                            chord_hints_ui(ui, i18n, chord);
                        });
                        ui.add_space(notedeck::tokens::SPACING_SM);
                    }
                },
                Localization::default(),
            );

        harness.run();
        harness.snapshot("chord_hint_strips");
    }
}
