//! [`OPEN`]: open any reference — `headway:`, `agentium:`, `notebook:`,
//! `nostr:` — by name, from a key, the Cmd-K bar or the socket (`nd call
//! open reference=…`), and bring notedeck to the front so a call from
//! a terminal is seen.
//!
//! It raises [`AppAction::Open`], so the open takes the same path a click on
//! the reference's chip does: resolution through the registered
//! [`ReferenceParser`](notedeck::ReferenceParser)s, the chrome's retry of a
//! reference that doesn't resolve yet, and the target app's nav entry.

use super::Chrome;
use notedeck::{
    AppAction, AppContext, OpenUri, ToolArg, ToolArgType, ToolInvocation, ToolOutcome, ToolSpec,
};

/// Open a reference in the app that owns it, and raise the window.
pub const OPEN: &str = "open";

/// The open tool, for the host's tool registry.
pub(super) fn open_tools() -> [ToolSpec; 1] {
    [ToolSpec::new(
        OPEN,
        "Open a reference (headway:<board>/<word-id>, agentium:<word-id>, \
         notebook:…, nostr:…) in the app that owns it, as a click on its chip \
         would, and bring notedeck to the front.",
        vec![
            ToolArg::new(
                "reference",
                ToolArgType::String,
                "The reference to open, optionally with a ?msg=… query.",
            )
            .required(true),
            ToolArg::new(
                "msg",
                ToolArgType::String,
                "A message for the opened entity, e.g. a prompt to send into an agentium session.",
            ),
        ],
    )
    .titled("Open reference")]
}

impl Chrome {
    /// Run [`OPEN`], or `None` for another tool. An argument that
    /// names no reference a registered parser (or the profile / hashtag
    /// opener) recognizes is an [`Error`](ToolOutcome::Error); otherwise the
    /// window is raised and the open comes back as an
    /// [`Intent`](ToolOutcome::Intent) for the caller to route.
    pub(super) fn call_open_tool(
        &mut self,
        ctx: &mut AppContext,
        invocation: &ToolInvocation,
    ) -> Option<ToolOutcome> {
        if invocation.name != OPEN {
            return None;
        }
        Some(open_outcome(ctx, &invocation.args))
    }
}

/// What an [`OPEN`] call with `args` comes to.
fn open_outcome(ctx: &mut AppContext, args: &serde_json::Value) -> ToolOutcome {
    let Some(reference) = args["reference"].as_str() else {
        return ToolOutcome::Error("open needs a 'reference' string".to_owned());
    };
    let Some(mut open) = OpenUri::parse(reference) else {
        return ToolOutcome::Error("open: the reference is empty".to_owned());
    };
    let recognized = ctx
        .registries
        .reference_parsers
        .is_reference(&open.reference)
        || notedeck::reference_note_action(&open.reference).is_some();
    if !recognized {
        return ToolOutcome::Error(format!(
            "open: '{}' is not a reference notedeck can open",
            open.reference
        ));
    }
    if let Some(msg) = args["msg"].as_str() {
        open.msg = Some(msg.to_owned());
    }
    // Opened from a terminal, the window may be behind it; from a key or the
    // bar it is already in front, and raising it again is harmless.
    if let Some(egui) = ctx.egui {
        notedeck_ui::activate_app(egui);
    }
    ToolOutcome::Intent(AppAction::Open(open))
}
