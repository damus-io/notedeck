//! Dave's normal-mode keys in the shipped keymap: Dave reads no bare keys of
//! its own in normal mode, so every `dave.*` binding in `default_keymap.toml`
//! must name a UI tool the real Dave advertises, which the host's registry
//! routes to Dave.
//!
//! What each key then does is Dave's own tests' business
//! (`notedeck_dave`'s `ui/keybindings.rs` tests run chrome's engine on this
//! keymap ahead of Dave, and check each binding calls its tool from its pane
//! and Dave takes the call).

#![cfg(feature = "dave")]

use std::collections::BTreeSet;

use notedeck::{App, AppId, Notedeck, ToolOwner};
use notedeck_chrome::keymap::DEFAULT_KEYMAP;
use notedeck_chrome::{Chrome, NotedeckApp};
use notedeck_dave::Dave;

/// Dave's UI tools that read rather than act, for a macro step to hand on;
/// no key is bound to them.
const READ_ONLY_TOOLS: [&str; 1] = ["dave_session_issue"];

/// The scopes Dave reports in normal mode, by pane (`ui_tools::key_scopes`).
const DAVE_SCOPES: [&str; 3] = ["dave.chat", "dave.sessions", "dave.normal"];

/// One Dave `[[bind]]` of the shipped keymap, as written.
#[derive(Debug)]
struct Binding {
    scope: String,
    tool: String,
}

/// The shipped keymap's Dave bindings as written.
fn dave_bindings() -> Vec<Binding> {
    let table: toml::Table = DEFAULT_KEYMAP.parse().expect("the default keymap is TOML");
    let binds = table["bind"].as_array().expect("[[bind]] entries");
    binds
        .iter()
        .filter_map(|bind| {
            let scope = bind["scope"].as_str()?;
            if !scope.starts_with("dave") {
                return None;
            }
            let tool = bind.get("tool")?.as_str()?;
            Some(Binding {
                scope: scope.to_owned(),
                tool: tool.to_owned(),
            })
        })
        .collect()
}

/// Every Dave binding is in a scope Dave reports, and names a UI tool the
/// host's registry gives to Dave; between them they bind every tool Dave
/// advertises.
#[test]
fn every_dave_binding_names_a_dave_tool() {
    let dir = tempfile::TempDir::new().expect("tmp dir");
    let args = ["notedeck-test".to_owned(), "--testrunner".to_owned()];
    let egui_ctx = egui::Context::default();
    let mut notedeck = Notedeck::init(&egui_ctx, dir.path(), &args);
    let dave = {
        let ctx = notedeck.app_context();
        Dave::new(None, ctx.ndb.clone(), ctx.waker.clone(), ctx.path)
    };
    let advertised: BTreeSet<String> = dave
        .ui_tools()
        .iter()
        .map(|spec| spec.name().to_owned())
        .filter(|name| !READ_ONLY_TOOLS.contains(&name.as_str()))
        .collect();
    let mut chrome = Chrome::new_headless_with_apps(
        &mut notedeck,
        [NotedeckApp::Other("dave".to_owned(), Box::new(dave))],
    );
    // As the host does: the registry follows the running apps.
    let update = App::take_tool_update(&mut chrome).expect("the running set changed");
    notedeck.apply_tool_update(update);

    let bindings = dave_bindings();
    assert!(bindings.len() > 30, "Dave's keymap is in: {bindings:?}");
    let ctx = notedeck.app_context();
    let mut bound = BTreeSet::new();
    for Binding { scope, tool } in bindings {
        assert!(
            DAVE_SCOPES.contains(&scope.as_str()),
            "{scope} isn't a scope Dave reports"
        );
        let owner = ctx.registries.tools.ui_owner(&tool);
        assert!(
            matches!(owner, Some(ToolOwner::App(id)) if id == AppId(0)),
            "{scope} binds {tool}, which isn't Dave's UI tool ({owner:?})"
        );
        bound.insert(tool);
    }
    assert_eq!(bound, advertised, "every Dave UI tool that acts has a key");
}
