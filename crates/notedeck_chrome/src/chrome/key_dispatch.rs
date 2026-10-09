//! The chrome's keymap at work: each frame, before the active app renders,
//! resolve the key press against the app's key scopes and run the bound tool
//! (see [`crate::keymap`]).

use super::actions::chrome_handle_app_action;
use super::Chrome;
use crate::keymap::{KeyDispatch, KeySeq, Keymap, Scopes};
use notedeck::{App, AppAction, AppContext, ToolInvocation, ToolOutcome};

impl Chrome {
    /// Resolve this frame's key press against the active app's
    /// [key scopes](App::key_scopes) and [current ref](App::current_ref), and
    /// dispatch what it's bound to: a UI tool goes to the app that owns it, a
    /// data tool runs here and now. A key the keymap takes is swallowed, so
    /// the app rendering after this never sees it; any other key reaches the
    /// app untouched.
    #[profiling::function]
    pub(super) fn dispatch_keys(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) {
        let app = self.apps.get(self.active as usize);
        let scopes = Scopes::new(app.map_or(&[][..], |app| app.key_scopes()))
            .with_current(app.and_then(|app| app.current_ref()));

        // The engine is lent out while it asks `self` which tools are
        // available; an empty `KeyEngine` doesn't allocate.
        let mut keys = std::mem::take(&mut self.keys);
        let invocation = match keys.resolve(ui.ctx(), scopes, |name| {
            self.key_tool_available(&ctx.registries.tools, name)
        }) {
            None => None,
            Some(KeyDispatch::Tool(invocation)) => Some(invocation),
            Some(KeyDispatch::Macro(name)) => {
                tracing::warn!("keymap: macro bindings are not supported yet; '{name}' ignored");
                None
            }
        };
        self.keys = keys;

        let Some(invocation) = invocation else {
            return;
        };
        let action = if is_data_tool(ctx, &invocation.name) {
            call_data_tool(ctx, &invocation)
        } else {
            Some(AppAction::CallTool(invocation))
        };
        if let Some(action) = action {
            chrome_handle_app_action(self, ctx, action, ui);
        }
    }

    /// Whether the tool `name` a key is bound to would do something now: a
    /// UI tool its owner says is [available](Self::ui_tool_available), or any
    /// registered data tool (which acts on what its args name, so only the
    /// keymap's [`CURRENT`](crate::keymap::CURRENT) gate applies). Doesn't
    /// allocate.
    pub fn key_tool_available(&self, tools: &notedeck::ToolRegistry, name: &str) -> bool {
        self.ui_tool_available(tools, name) || is_registered_data_tool(tools, name)
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

/// Whether `name` is an app's data tool in `tools`, not a UI tool (nor the
/// chrome's own, which never registers).
fn is_registered_data_tool(tools: &notedeck::ToolRegistry, name: &str) -> bool {
    tools.contains(name) && tools.ui_owner(name).is_none()
}

/// Whether the key's tool `name` is an app's data tool, to run here rather
/// than route to an app.
fn is_data_tool(ctx: &AppContext, name: &str) -> bool {
    is_registered_data_tool(&ctx.registries.tools, name)
}

/// Run a data tool a key is bound to, returning the intent it raised, if
/// any, for the chrome to route. Its data is for an agent; a key only wants
/// its effect (a board edit, which the app picks up as it folds in), so only
/// a failure is worth a line in the log.
fn call_data_tool(ctx: &mut AppContext, invocation: &ToolInvocation) -> Option<AppAction> {
    match ctx.call_tool(&invocation.name, &invocation.args) {
        ToolOutcome::Data(_) => None,
        ToolOutcome::Intent(action) => Some(action),
        ToolOutcome::Error(err) => {
            tracing::warn!("keymap: '{}' failed: {err}", invocation.name);
            None
        }
    }
}
