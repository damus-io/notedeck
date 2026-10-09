//! The chrome's keymap engine through real chrome frames: a fake app reports
//! the `fake` key scope and advertises UI tools, a test keymap binds keys to
//! them, and each test presses keys and checks which tools ran and which keys
//! still reached the app.

mod common;

use common::{field_id, Fixture};
use egui::{Key, Modifiers};
use notedeck_ui::chord::CHORD_TIMEOUT;
use serde_json::json;

/// The test keymap: one binding per behaviour under test.
const KEYMAP: &str = r#"
[[bind]]
scope = "fake"
keys = "j"
tool = "fake_down"

[[bind]]
scope = "fake"
keys = "g g"
tool = "fake_first"

[[bind]]
scope = "fake"
keys = "<C-f>"
tool = "fake_page"

[[bind]]
scope = "fake"
keys = "x"
tool = "fake_gated"

[[bind]]
scope = "elsewhere"
keys = "k"
tool = "fake_down"

[[bind]]
scope = "fake"
keys = "m"
tool = "fake_mark"
args = { card = "{current}", note = "on {current}", n = 1 }
"#;

/// A [`Fixture`] with [`KEYMAP`] loaded.
fn fixture() -> Fixture {
    Fixture::new(KEYMAP)
}

#[tokio::test]
async fn a_bound_key_runs_its_tool_and_never_reaches_the_app() {
    let mut f = fixture();
    f.key(Key::J);
    assert_eq!(f.calls(), ["fake_down"]);
    assert_eq!(f.seen(), [], "the keymap swallowed j");
}

#[tokio::test]
async fn a_chord_runs_on_its_last_key() {
    let mut f = fixture();
    f.key(Key::G);
    assert_eq!(f.calls(), Vec::<String>::new(), "g alone runs nothing");
    assert_eq!(f.pending().as_deref(), Some("g"));
    assert_eq!(f.seen(), [], "the chord's first key is swallowed too");

    f.key(Key::G);
    assert_eq!(f.calls(), ["fake_first"]);
    assert_eq!(f.pending(), None);
}

#[tokio::test]
async fn a_chord_lapses_after_its_timeout() {
    let mut f = fixture();
    f.key(Key::G);
    f.idle(CHORD_TIMEOUT + 0.1);
    assert_eq!(f.pending(), None);

    // The next g starts a new chord rather than finishing the old one.
    f.key(Key::G);
    assert_eq!(f.calls(), Vec::<String>::new());
    assert_eq!(f.pending().as_deref(), Some("g"));
}

#[tokio::test]
async fn a_chord_broken_by_a_bound_key_runs_that_key() {
    let mut f = fixture();
    f.key(Key::G);
    f.key(Key::J);
    assert_eq!(f.calls(), ["fake_down"]);
    assert_eq!(f.pending(), None);
}

/// Esc only cancels a chord, as in vim: it runs nothing, and the app never
/// sees it. With no chord pending it's the app's.
#[tokio::test]
async fn esc_cancels_a_pending_chord() {
    let mut f = fixture();
    f.key(Key::G);
    f.key(Key::Escape);
    assert_eq!(f.calls(), Vec::<String>::new());
    assert_eq!(f.pending(), None);
    assert_eq!(f.seen(), [], "the Esc only dropped the g");

    f.key(Key::Escape);
    assert_eq!(f.seen(), [Key::Escape]);
}

#[tokio::test]
async fn a_focused_text_field_keeps_its_keys() {
    let mut f = fixture();
    f.state.borrow_mut().show_field = true;
    f.frame(Vec::new());
    f.egui_ctx.memory_mut(|m| m.request_focus(field_id()));
    f.frame(Vec::new());

    f.press(Modifiers::NONE, Key::J, Some("j"));
    assert_eq!(f.calls(), Vec::<String>::new());
    assert_eq!(f.state.borrow().text, "j", "j typed into the field");
}

#[tokio::test]
async fn an_unavailable_tool_lets_its_key_through() {
    let mut f = fixture();
    f.state.borrow_mut().gated_available = false;
    f.key(Key::X);
    assert_eq!(f.calls(), Vec::<String>::new());
    assert_eq!(f.seen(), [Key::X]);

    f.state.borrow_mut().gated_available = true;
    f.key(Key::X);
    assert_eq!(f.calls(), ["fake_gated"]);
    assert_eq!(f.seen(), [Key::X], "the available one was swallowed");
}

#[tokio::test]
async fn an_unbound_key_reaches_the_app() {
    let mut f = fixture();
    // `k` is bound, but only in a scope the app doesn't report.
    f.key(Key::K);
    assert_eq!(f.calls(), Vec::<String>::new());
    assert_eq!(f.seen(), [Key::K]);
}

#[tokio::test]
async fn a_modified_key_matches_only_a_modified_binding() {
    let mut f = fixture();
    f.press(Modifiers::CTRL | Modifiers::COMMAND, Key::J, None);
    assert_eq!(f.calls(), Vec::<String>::new(), "Ctrl+j is not j");
    assert_eq!(f.seen(), [Key::J]);

    f.key(Key::F);
    assert_eq!(f.calls(), Vec::<String>::new(), "f is not Ctrl+f");

    f.press(Modifiers::CTRL | Modifiers::COMMAND, Key::F, None);
    assert_eq!(f.calls(), ["fake_page"]);
    assert_eq!(f.seen(), [Key::J, Key::F]);
}

/// A key bound to a data tool runs it at once, its args' `{current}` the
/// app's current ref, wherever it appears in a string; no UI tool runs and
/// the app never sees the key.
#[tokio::test]
async fn a_data_tool_binding_runs_on_the_current_ref() {
    let mut f = fixture();
    f.state.borrow_mut().current = Some("fake:one".to_owned());
    f.frame(Vec::new());
    f.key(Key::M);
    assert_eq!(
        f.marked(),
        [json!({ "card": "fake:one", "note": "on fake:one", "n": 1 })]
    );
    assert_eq!(f.calls(), Vec::<String>::new());
    assert_eq!(f.seen(), [], "the keymap swallowed m");
}

/// Without a current ref, a binding naming `{current}` is unavailable and
/// its key reaches the app.
#[tokio::test]
async fn without_a_current_ref_its_binding_falls_through() {
    let mut f = fixture();
    f.key(Key::M);
    assert_eq!(f.marked(), Vec::<serde_json::Value>::new());
    assert_eq!(f.seen(), [Key::M]);
}
