//! The which-key strip: while a leader chord is pending, a row of keycaps
//! above the chat input shows what the chord will accept next.

use super::keybind_hint::KeybindHint;
use super::keybindings::{KeyAction, Pending};
use egui::{Align, Layout};
use notedeck::{tr, Localization};

/// Height of a keycap in the strip.
const KEYCAP: f32 = 18.0;

/// Extra keycap width per character past the first, so `za` and `gg` fit.
const KEYCAP_PER_CHAR: f32 = 8.0;

/// Height the strip reserves in the bottom-up input stack.
pub const STRIP_HEIGHT: f32 = KEYCAP + notedeck::tokens::SPACING_XS;

/// Draw the keycaps `pending` accepts, grouped and left-aligned.
///
/// Runs every frame a chord is pending, so it only walks the static hint
/// tables; the one allocation, a keycap's tooltip, happens on hover.
pub fn chord_hints_ui(ui: &mut egui::Ui, i18n: &mut Localization, pending: Pending) {
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        for (i, group) in pending.hints().iter().enumerate() {
            if i > 0 {
                ui.add_space(notedeck::tokens::SPACING_MD);
            }
            for hint in *group {
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
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Vec2;
    use egui_kittest::Harness;

    /// Every chord state's strip, one per row: `<leader>`, then `z`, then `g`.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_chord_hint_strips() {
        let mut harness = Harness::builder()
            .with_size(Vec2::new(420.0, 90.0))
            .renderer(notedeck::software_renderer())
            .build_ui_state(
                |ui, i18n: &mut Localization| {
                    for pending in [Pending::Leader, Pending::LeaderZ, Pending::LeaderG] {
                        ui.allocate_ui(egui::vec2(ui.available_width(), STRIP_HEIGHT), |ui| {
                            chord_hints_ui(ui, i18n, pending);
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
