//! Editing the account's keymap overrides inside notedeck: two of the
//! chrome's own UI tools. [`EDIT_KEYMAP`] opens Notebook's keymap editor on
//! the stored overrides (or [`KEYMAP_TEMPLATE`]); [`SAVE_KEYMAP_TOOL`], which
//! the editor raises, merges and saves them through
//! [`Chrome::save_user_keymap`] and tells the editor how it went.

use super::Chrome;
use notedeck::{
    AppContext, ToolArg, ToolArgType, ToolInvocation, ToolOutcome, ToolSpec, SAVE_KEYMAP_TOOL,
};

/// Opens the keymap editor on the account's keymap overrides. In the side
/// menu as "Keymap"; unbound by default (see `default_keymap.toml`).
pub const EDIT_KEYMAP: &str = "chrome_edit_keymap";

/// What the keymap editor opens on for an account with no overrides yet: the
/// format, in comments, so it saves as an empty keymap until something is
/// uncommented. A user keymap is a delta over the built-in one, so this is
/// not the defaults.
pub const KEYMAP_TEMPLATE: &str = r#"# Your keymap: bindings and macros merged over notedeck's built-in keymap,
# saved privately to your account and synced to your other devices.
#
# A binding names a key scope, the keys, and the tool (or macro) they run.
# Keys are space-separated: `g g`, `E` (Shift = uppercase), `<Enter>`,
# `<Esc>`, `<Space>`, `<C-f>`, `<S-Space>`. A binding of the same scope and
# keys as a built-in one replaces it. Press `?` in an app to see its keys.
#
# [[bind]]
# scope = "headway.grid"
# keys  = "g g"
# tool  = "headway_cursor_first"
# label = "first card"      # shown in the which-key strip
# group = "move"
#
# `unbind = true` drops a built-in binding:
#
# [[bind]]
# scope = "dave.normal"
# keys  = "E"
# unbind = true
#
# A binding in the "global" scope works in every app that doesn't bind the
# same keys itself, e.g. this editor on a key:
#
# [[bind]]
# scope = "global"
# keys  = "g K"
# tool  = "chrome_edit_keymap"
# label = "edit keymap"
#
# A macro runs tools in turn, each once the last has settled; bind it with
# `macro = "<name>"` in place of `tool`:
#
# [macro.session-explainer]
# label = "card explainer"
# steps = [ { tool = "dave_open_issue" }, { tool = "headway_open_explainer" } ]
#
# A step waits for its tool to be offered, and the run aborts if it never is.
# This one opens the review queue of the epic above a session's card, and
# stops on a card with no parent:
#
# [macro.session-epic-review]
# label = "epic review queue"
# steps = [ { tool = "dave_open_issue" }, { tool = "headway_open_parent" }, { tool = "headway_open_review_queue" } ]
"#;

/// The keymap editor's tools, for the host's tool registry.
pub(super) fn keymap_editor_tools() -> [ToolSpec; 2] {
    [
        ToolSpec::new(
            EDIT_KEYMAP,
            "Open the editor for the account's keymap overrides.",
            Vec::new(),
        ),
        ToolSpec::new(
            SAVE_KEYMAP_TOOL,
            "Save the account's keymap overrides, merged over the built-in keymap.",
            vec![ToolArg::new(
                "toml",
                ToolArgType::String,
                "The user keymap, in the keymap's TOML format.",
            )
            .required(true)],
        ),
    ]
}

impl Chrome {
    /// Run the keymap editor's tool `invocation`, or `None` when it isn't
    /// one.
    pub(super) fn call_keymap_editor_tool(
        &mut self,
        ctx: &mut AppContext,
        invocation: &ToolInvocation,
    ) -> Option<ToolOutcome> {
        let outcome = match invocation.name.as_str() {
            EDIT_KEYMAP => match self.edit_keymap(ctx) {
                Ok(()) => ToolOutcome::Data(serde_json::Value::Null),
                Err(err) => ToolOutcome::Error(err),
            },
            SAVE_KEYMAP_TOOL => {
                let Some(toml) = invocation.args.get("toml").and_then(|v| v.as_str()) else {
                    return Some(ToolOutcome::Error("missing the 'toml' argument".to_owned()));
                };
                match self.save_keymap_from_editor(ctx, toml) {
                    Ok(()) => ToolOutcome::Data(serde_json::Value::Null),
                    Err(err) => ToolOutcome::Error(err),
                }
            }
            _ => return None,
        };
        Some(outcome)
    }

    /// Whether the keymap editor's tool `name` would do something now, or
    /// `None` when it isn't one. Opening the editor needs the Notebook app.
    pub(super) fn keymap_editor_tool_available(&self, name: &str) -> Option<bool> {
        match name {
            EDIT_KEYMAP => Some(self.has_notebook()),
            SAVE_KEYMAP_TOOL => Some(true),
            _ => None,
        }
    }

    /// Whether the Notebook app, which hosts the keymap editor, is built in.
    fn has_notebook(&self) -> bool {
        #[cfg(feature = "notebook")]
        return self
            .apps
            .iter()
            .any(|app| matches!(app, crate::NotedeckApp::Notebook(_)));
        #[cfg(not(feature = "notebook"))]
        false
    }

    /// Switch to Notebook and open its keymap editor on the selected
    /// account's overrides, or [`KEYMAP_TEMPLATE`] when it has none, with
    /// why the stored ones didn't load if they didn't. The caret lands in
    /// the editor. An error when Notebook isn't built in.
    pub fn edit_keymap(&mut self, ctx: &mut AppContext) -> Result<(), String> {
        #[cfg(feature = "notebook")]
        {
            let author = *ctx.accounts.selected_account_pubkey();
            let toml = crate::keymap::user::load_user_keymap(ctx.ndb, &author)
                .unwrap_or_else(|| KEYMAP_TEMPLATE.to_owned());
            let error = self.keymap_error().map(str::to_owned);
            let Some(slot) = self
                .apps
                .iter()
                .position(|app| matches!(app, crate::NotedeckApp::Notebook(_)))
            else {
                return Err("the Notebook app isn't running".to_owned());
            };
            self.app_focus.land_autofocused(slot);
            self.set_active(slot as i32);
            if let Some(notebook) = self.get_notebook_app() {
                notebook.edit_keymap(toml, error);
            }
            Ok(())
        }
        #[cfg(not(feature = "notebook"))]
        {
            let _ = ctx;
            Err("the keymap editor needs the Notebook app, which isn't built in".to_owned())
        }
    }

    /// [`save_user_keymap`](Self::save_user_keymap), then tell Notebook's
    /// keymap editor how it went, so a refusal shows there with its reason
    /// and a save marks it clean.
    fn save_keymap_from_editor(&mut self, ctx: &mut AppContext, toml: &str) -> Result<(), String> {
        let result = self.save_user_keymap(ctx, toml);
        #[cfg(feature = "notebook")]
        {
            let warnings = self.keymap().warnings().to_vec();
            if let Some(notebook) = self.get_notebook_app() {
                let answer = match &result {
                    Ok(()) => Ok(warnings.as_slice()),
                    Err(err) => Err(err.as_str()),
                };
                notebook.keymap_saved(toml, answer);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_is_an_empty_keymap() {
        let keymap =
            crate::keymap::parse_over(crate::keymap::DEFAULT_KEYMAP, KEYMAP_TEMPLATE, |_| true)
                .expect("the template parses");
        assert_eq!(
            keymap.bindings(crate::keymap::GLOBAL_SCOPE).len(),
            crate::keymap::default_keymap()
                .bindings(crate::keymap::GLOBAL_SCOPE)
                .len(),
            "every binding in it is commented out"
        );
    }

    #[test]
    fn the_template_s_examples_parse_once_uncommented() {
        let uncommented: String = KEYMAP_TEMPLATE
            .lines()
            .filter_map(|line| line.strip_prefix("# "))
            .filter(|line| {
                line.starts_with('[')
                    || (line.starts_with(|c: char| c.is_ascii_lowercase()) && line.contains('='))
            })
            .collect::<Vec<_>>()
            .join("\n");
        crate::keymap::parse_over(crate::keymap::DEFAULT_KEYMAP, &uncommented, |_| true)
            .unwrap_or_else(|err| panic!("{err}\n{uncommented}"));
    }
}
