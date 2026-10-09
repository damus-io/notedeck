//! The chrome's keymap at work: each frame, before the active app renders,
//! resolve the key press against the app's key scopes and run the bound UI
//! tool (see [`crate::keymap`]).

use super::actions::chrome_handle_app_action;
use super::Chrome;
use crate::keymap::{KeyDispatch, KeySeq, Keymap};
use notedeck::{App, AppAction, AppContext};

impl Chrome {
    /// Resolve this frame's key press against the active app's
    /// [key scopes](App::key_scopes) and dispatch what it's bound to. A key
    /// the keymap takes is swallowed, so the app rendering after this never
    /// sees it; any other key reaches the app untouched.
    #[profiling::function]
    pub(super) fn dispatch_keys(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) {
        let scopes = self
            .apps
            .get(self.active as usize)
            .map_or(&[][..], |app| app.key_scopes());

        // The engine is lent out while it asks `self` which tools are
        // available; an empty `KeyEngine` doesn't allocate.
        let mut keys = std::mem::take(&mut self.keys);
        let action = match keys.resolve(ui.ctx(), scopes, |name| {
            self.ui_tool_available(&ctx.registries.tools, name)
        }) {
            None => None,
            Some(KeyDispatch::Tool(invocation)) => Some(AppAction::CallTool(invocation.clone())),
            Some(KeyDispatch::Macro(name)) => {
                tracing::warn!("keymap: macro bindings are not supported yet; '{name}' ignored");
                None
            }
        };
        self.keys = keys;

        if let Some(action) = action {
            chrome_handle_app_action(self, ctx, action, ui);
        }
    }

    /// The keymap the chrome resolves keys against.
    pub fn keymap(&self) -> &Keymap {
        self.keys.keymap()
    }

    /// The keys of a chord in progress (the `g` of `g g`), if any.
    pub fn pending_keys(&self) -> Option<KeySeq> {
        self.keys.pending()
    }

    /// Replace the keymap, e.g. with one parsed from a user's overrides. Any
    /// chord in progress is dropped.
    pub fn set_keymap(&mut self, keymap: Keymap) {
        self.keys.set_keymap(keymap);
    }
}
