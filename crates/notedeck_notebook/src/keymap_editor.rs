//! The keymap editor: a full-screen TOML editor for the account's keymap
//! overrides, the sibling of the longform [`editor`](crate::editor) for a
//! document that isn't markdown and isn't the notebook's own.
//!
//! The chrome owns the keymap. It opens this editor through
//! [`Notebook::edit_keymap`](crate::Notebook::edit_keymap), seeded with the
//! stored overrides, and a save goes back to it as a call to its
//! [`SAVE_KEYMAP_TOOL`]: the chrome merges the text over the built-in keymap,
//! writes it as the account's private keymap note, and answers through
//! [`Notebook::keymap_saved`](crate::Notebook::keymap_saved), so a keymap that
//! doesn't merge stays open here with the reason under the header.

use egui::{Key, Layout, Modifiers, RichText, ScrollArea, TextEdit};
use notedeck::{AppAction, ColorTheme, SAVE_KEYMAP_TOOL, ToolInvocation};

/// The id of the editor's text field, so opening the editor can focus it.
pub(crate) fn source_id() -> egui::Id {
    egui::Id::unique("notebook-keymap-source")
}

/// The open keymap editor: the working text, the text last saved, and what
/// the chrome said about the last save or the stored keymap.
pub(crate) struct KeymapEditor {
    /// The TOML being edited.
    pub text: String,
    /// The text the chrome last accepted (or was opened with), to tell
    /// unsaved changes.
    saved: String,
    /// Why the last save was refused, or why the stored keymap didn't load.
    error: Option<String>,
    /// Lints on the keymap the last save put in use (an unbind that matches
    /// nothing, a shadowed prefix): it saved, but may not do what was meant.
    warnings: Vec<String>,
    /// Close once the save in flight is accepted (Close with unsaved changes
    /// saves first, and stays open if the save is refused).
    close_on_save: bool,
    /// Put the caret in the text on the next frame: the editor was just
    /// opened, by a key or a menu, to type in.
    focus: bool,
}

impl KeymapEditor {
    /// An editor on `text`, the stored overrides or a template for an account
    /// with none, clean. `error` is why the stored overrides didn't load, if
    /// they didn't.
    pub(crate) fn new(text: String, error: Option<String>) -> Self {
        KeymapEditor {
            saved: text.clone(),
            text,
            error,
            warnings: Vec::new(),
            close_on_save: false,
            focus: true,
        }
    }

    /// Whether the text differs from what was last saved.
    pub(crate) fn dirty(&self) -> bool {
        self.text != self.saved
    }

    /// Take the chrome's answer to saving `toml`: on success it is the new
    /// clean baseline, with `warnings` to show; on failure the reason is shown
    /// and the text stays as typed. Returns whether the editor should close
    /// (it was saved on Close).
    pub(crate) fn saved(&mut self, toml: &str, result: Result<&[String], &str>) -> bool {
        let close = std::mem::take(&mut self.close_on_save);
        match result {
            Ok(warnings) => {
                toml.clone_into(&mut self.saved);
                self.error = None;
                self.warnings = warnings.to_vec();
                close
            }
            Err(err) => {
                self.error = Some(err.to_owned());
                self.warnings.clear();
                false
            }
        }
    }

    /// The call that saves the working text, for the chrome.
    fn save_action(&self) -> AppAction {
        AppAction::CallTool(ToolInvocation::new(
            SAVE_KEYMAP_TOOL,
            serde_json::json!({ "toml": self.text }),
        ))
    }
}

/// What the keymap editor asks of the app this frame.
pub(crate) enum KeymapEditorResponse {
    /// Nothing.
    None,
    /// Raise this action for the chrome: a save.
    Action(AppAction),
    /// Leave the editor; there is nothing unsaved.
    Close,
}

/// Render the keymap editor: a header (back / title / dirty marker / Save),
/// the chrome's verdict on the last save, and the TOML source filling the
/// rest. Save (or Ctrl+S) with unsaved changes raises the chrome's save tool;
/// Back (or Esc) with unsaved changes saves first and closes once that's
/// accepted.
pub(crate) fn keymap_editor_ui(
    editor: &mut KeymapEditor,
    ui: &mut egui::Ui,
) -> KeymapEditorResponse {
    use notedeck::tokens::{SPACING_LG, SPACING_SM};
    let theme = ColorTheme::current(ui.ctx());
    // Read before the text field, which would otherwise see the S.
    let save_key = ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::S));
    let mut save = false;
    // Esc leaves the editor, as Back does. egui has already dropped the
    // text field's focus for it by now, so only another widget's focus (a
    // popup's) keeps it.
    let mut close = ui.memory(|m| m.focused().is_none())
        && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
    if std::mem::take(&mut editor.focus) {
        ui.memory_mut(|m| m.request_focus(source_id()));
    }

    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(SPACING_LG as i8, SPACING_SM as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                close |= ui.button("← Back").clicked();
                ui.label(RichText::new("Keymap").heading().color(theme.text_primary));
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    let dirty = editor.dirty();
                    save = ui.add_enabled(dirty, egui::Button::new("Save")).clicked();
                    if dirty {
                        ui.label(RichText::new("●").color(theme.accent))
                            .on_hover_text("Unsaved changes");
                    }
                });
            });
            ui.label(
                RichText::new(
                    "Bindings and macros merged over the built-in keymap, \
                     saved privately to your account and synced to your devices.",
                )
                .small()
                .color(theme.text_muted),
            );

            ui.add_space(SPACING_SM);
            status_ui(editor, ui, &theme);
            ui.separator();
            ui.add_space(SPACING_SM);

            ScrollArea::vertical()
                .id_salt("notebook-keymap-body")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let resp = ui
                        .add(
                            TextEdit::multiline(&mut editor.text)
                                .id(source_id())
                                .code_editor()
                                .frame(egui::Frame::NONE)
                                .desired_width(f32::INFINITY)
                                .desired_rows(24),
                        )
                        // No label or placeholder of its own to be read by.
                        .accessible_name("Keymap TOML");
                    // Opened from another app, the chrome's focus restore
                    // runs a frame later; this claims focus then.
                    notedeck_ui::autofocus(&resp, ui.ctx());
                });
        });

    if !editor.dirty() {
        return if close {
            KeymapEditorResponse::Close
        } else {
            KeymapEditorResponse::None
        };
    }
    if close {
        editor.close_on_save = true;
    }
    if save || save_key || close {
        return KeymapEditorResponse::Action(editor.save_action());
    }
    KeymapEditorResponse::None
}

/// The chrome's verdict: the error that kept the last save (or the stored
/// keymap) out, else the warnings on the keymap the last save put in use.
fn status_ui(editor: &KeymapEditor, ui: &mut egui::Ui, theme: &ColorTheme) {
    if let Some(error) = &editor.error {
        ui.label(
            RichText::new(error.as_str())
                .monospace()
                .color(theme.destructive),
        );
        return;
    }
    for warning in &editor.warnings {
        ui.label(
            RichText::new(warning.as_str())
                .monospace()
                .color(theme.warning),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_save_keeps_the_text_dirty_and_says_why() {
        let mut editor = KeymapEditor::new("a".to_owned(), None);
        assert!(!editor.dirty());
        editor.text.push('b');
        assert!(editor.dirty());

        assert!(!editor.saved("ab", Err("bad")));
        assert!(editor.dirty(), "a refused save leaves the change unsaved");
        assert_eq!(editor.error.as_deref(), Some("bad"));

        let lints = ["shadowed".to_owned()];
        assert!(!editor.saved("ab", Ok(&lints)));
        assert!(!editor.dirty());
        assert_eq!(editor.error, None);
        assert_eq!(editor.warnings, lints);
    }

    #[test]
    fn close_with_changes_closes_only_once_the_save_lands() {
        let mut editor = KeymapEditor::new(String::new(), None);
        editor.text.push('x');
        editor.close_on_save = true;
        assert!(!editor.saved("x", Err("bad")), "refused: stay open");
        editor.close_on_save = true;
        assert!(editor.saved("x", Ok(&[])), "accepted: close");
    }
}
