//! The chrome's key decision: whether a tool a key is bound to would do
//! anything now, and, for the key that fires, where its tool runs.
//!
//! The chrome asks it each frame in
//! [`Chrome::dispatch_keys`](crate::Chrome), and its which-key strip asks the
//! same availability question. A test harness that stands in for the chrome
//! around one app asks it too, through its own [`KeyTools`], so its keys go
//! where the chrome's would without a copy of the chrome's routing.

use notedeck::{ToolInvocation, ToolRegistry};

use super::{KeyDispatch, KeyEngine, Scopes};

/// Where a key press's tool runs, as the chrome routes it.
#[derive(Debug, PartialEq)]
pub enum KeyTarget {
    /// One of the chrome's own tools (`?`, the strip's Esc, Ctrl-O / Ctrl-I,
    /// the command bar's, ...): the chrome runs it.
    Chrome(ToolInvocation),
    /// An app's UI tool: the app that owns it runs it.
    Ui(ToolInvocation),
    /// An app's data tool: the chrome runs it here and now, whichever app is
    /// in front.
    Data(ToolInvocation),
    /// The macro with this name.
    Macro(String),
}

/// What the key decision asks of the tools a key can be bound to. The chrome
/// answers from its own state and the host's tool registry; a harness
/// answers for the one app it hosts, with the chrome's rules for the
/// chrome's own tools ([`key_hints_tool_available`](crate::key_hints_tool_available),
/// [`jump_tool_available`](crate::jump_tool_available)).
pub trait KeyTools {
    /// Whether the chrome's own tool `name` would do something now, or
    /// `None` when `name` isn't one of the chrome's.
    fn chrome_tool_available(&self, name: &str) -> Option<bool>;

    /// Whether an app's UI tool `name` would do something now: an app that
    /// is running owns it and says it is
    /// [available](notedeck::App::ui_tool_available).
    fn ui_tool_available(&self, name: &str) -> bool;

    /// Whether `name` is an app's data tool (see [`is_data_tool`]).
    fn is_data_tool(&self, name: &str) -> bool;

    /// Whether a key bound to the tool `name` fires now: the chrome's own
    /// tool when it says so, an app's UI tool when its owner says so, and
    /// any data tool, which acts on what its args name (so only the keymap's
    /// [`CURRENT`](super::CURRENT) gate applies). A key bound to anything
    /// else falls through. Doesn't allocate.
    fn key_tool_available(&self, name: &str) -> bool {
        match self.chrome_tool_available(name) {
            Some(available) => available,
            None => self.ui_tool_available(name) || self.is_data_tool(name),
        }
    }

    /// Where the tool `invocation` names runs, for a key that fired it.
    fn key_target(&self, invocation: ToolInvocation) -> KeyTarget {
        let name = invocation.name.as_str();
        if self.chrome_tool_available(name).is_some() {
            KeyTarget::Chrome(invocation)
        } else if self.is_data_tool(name) {
            KeyTarget::Data(invocation)
        } else {
            KeyTarget::Ui(invocation)
        }
    }
}

/// Whether `name` is an app's data tool in `tools`, not a UI tool (nor the
/// chrome's own, which never registers). Doesn't allocate.
pub fn is_data_tool(tools: &ToolRegistry, name: &str) -> bool {
    tools.contains(name) && tools.ui_owner(name).is_none()
}

impl KeyEngine {
    /// [`resolve`](Self::resolve) this frame's key press against `scopes`,
    /// with `tools` saying which bound tools are available, and say where
    /// what it fired runs. [`fired_jump`](Self::fired_jump) is set as
    /// `resolve` sets it. A frame with no key press doesn't allocate.
    pub fn resolve_target(
        &mut self,
        ctx: &egui::Context,
        scopes: Scopes<'_>,
        tools: &impl KeyTools,
    ) -> Option<KeyTarget> {
        match self.resolve(ctx, scopes, |name| tools.key_tool_available(name))? {
            KeyDispatch::Tool(invocation) => Some(tools.key_target(invocation)),
            KeyDispatch::Macro(name) => Some(KeyTarget::Macro(name.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tools for one app: `?` the chrome's, `ui` its UI tool, `data` its
    /// data tool, with `ui` available as set.
    struct Fake {
        ui_available: bool,
    }

    impl KeyTools for Fake {
        fn chrome_tool_available(&self, name: &str) -> Option<bool> {
            (name == "chrome").then_some(false)
        }
        fn ui_tool_available(&self, name: &str) -> bool {
            name == "ui" && self.ui_available
        }
        fn is_data_tool(&self, name: &str) -> bool {
            name == "data"
        }
    }

    fn call(name: &str) -> ToolInvocation {
        ToolInvocation {
            name: name.to_owned(),
            args: serde_json::Value::Null,
        }
    }

    #[test]
    fn the_chromes_answer_wins_and_a_data_tool_is_always_available() {
        let tools = Fake {
            ui_available: false,
        };
        assert!(!tools.key_tool_available("chrome"));
        assert!(!tools.key_tool_available("ui"));
        assert!(tools.key_tool_available("data"));
        assert!(!tools.key_tool_available("nothing"));
        assert!(Fake { ui_available: true }.key_tool_available("ui"));
    }

    #[test]
    fn a_fired_tool_runs_where_it_belongs() {
        let tools = Fake { ui_available: true };
        assert_eq!(
            tools.key_target(call("chrome")),
            KeyTarget::Chrome(call("chrome"))
        );
        assert_eq!(
            tools.key_target(call("data")),
            KeyTarget::Data(call("data"))
        );
        assert_eq!(tools.key_target(call("ui")), KeyTarget::Ui(call("ui")));
    }
}
