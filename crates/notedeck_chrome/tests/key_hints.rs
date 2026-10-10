//! The chrome's which-key strip through real chrome frames: the fake app's
//! bindings plus the shipped global ones (`?`, Esc), toggled, narrowed to a
//! pending chord's continuations, and every keycap it shows pressed back
//! through the keymap to check it runs what its label says.

mod common;

use common::Fixture;
use egui::{Key, KeyboardShortcut, Modifiers};
use notedeck_chrome::keymap::DEFAULT_KEYMAP;
use notedeck_chrome::{HIDE_KEY_HINTS, TOGGLE_KEY_HINTS};

/// The fake app's bindings, labelled and grouped for the strip.
const FAKE_KEYMAP: &str = r#"
[[bind]]
scope = "fake"
keys = "j"
tool = "fake_down"
label = "down"
group = "move"

[[bind]]
scope = "fake"
keys = "g g"
tool = "fake_first"
label = "first"
group = "move"

[[bind]]
scope = "fake"
keys = "<C-f>"
tool = "fake_page"
label = "page"
group = "move"

[[bind]]
scope = "fake"
keys = "x"
tool = "fake_gated"
label = "gated"
group = "act"
"#;

/// Which tool each label in the strip stands for.
const LABEL_TOOLS: &[(&str, &str)] = &[
    ("down", "fake_down"),
    ("first", "fake_first"),
    ("page", "fake_page"),
    ("gated", "fake_gated"),
    ("keys", TOGGLE_KEY_HINTS),
    ("hide keys", HIDE_KEY_HINTS),
];

/// A [`Fixture`] with the shipped keymap and [`FAKE_KEYMAP`] loaded.
fn fixture() -> Fixture {
    Fixture::new(&format!("{DEFAULT_KEYMAP}\n{FAKE_KEYMAP}"))
}

/// Press `?` the way a US layout reports it.
fn question_mark(f: &mut Fixture) {
    f.press(Modifiers::SHIFT, Key::Questionmark, Some("?"));
}

/// One keycap on the strip and the label it stands beside.
#[derive(Debug, PartialEq)]
struct ShownHint {
    keycap: String,
    label: String,
}

impl ShownHint {
    /// The keycap `keycap` beside `label`.
    fn new(keycap: &str, label: &str) -> Self {
        Self {
            keycap: keycap.to_owned(),
            label: label.to_owned(),
        }
    }
}

/// The strip's hints, one per keycap, or `None` if hidden.
fn shown(f: &mut Fixture) -> Option<Vec<ShownHint>> {
    let ctx = f.notedeck.app_context();
    let hints = f.chrome.key_hints(&ctx.registries.tools)?;
    Some(
        hints
            .flat_map(|hint| {
                hint.keycap_iter()
                    .map(move |cap| ShownHint::new(cap, hint.label))
            })
            .collect(),
    )
}

#[tokio::test]
async fn question_mark_toggles_the_strip() {
    let mut f = fixture();
    assert!(!f.chrome.key_hints_pinned());
    assert!(!f.drew("down"));

    question_mark(&mut f);
    assert!(f.chrome.key_hints_pinned());
    assert!(f.seen().is_empty(), "the keymap took ?");
    // Painted: labels and keycaps from the app's scope and the global one.
    for text in ["down", "first", "gg", "^f", "keys", "?", "hide keys", "esc"] {
        assert!(f.drew(text), "{text:?} not drawn in {:?}", f.drawn);
    }

    // The other way a layout can report `?`.
    f.press(Modifiers::SHIFT, Key::Slash, None);
    assert!(!f.chrome.key_hints_pinned());
    assert!(!f.drew("down"));
}

#[tokio::test]
async fn esc_hides_the_strip_and_only_then_is_taken() {
    let mut f = fixture();
    question_mark(&mut f);
    f.key(Key::Escape);
    assert!(!f.chrome.key_hints_pinned());
    assert!(!f.drew("down"));
    assert!(f.seen().is_empty(), "the keymap took Esc");

    // With nothing to hide, Esc goes to the app as before.
    f.key(Key::Escape);
    assert_eq!(f.seen(), [Key::Escape]);
}

#[tokio::test]
async fn a_pending_chord_shows_only_its_continuations() {
    let mut f = fixture();
    f.key(Key::G);
    assert_eq!(shown(&mut f), Some(vec![ShownHint::new("g", "first")]));
    assert!(f.drew("first") && f.drew("g"));
    assert!(!f.drew("down"));

    f.key(Key::G);
    assert_eq!(f.calls(), ["fake_first"]);
    assert_eq!(shown(&mut f), None, "gone once the chord completes");
    assert!(!f.drew("first"));

    // Over a pinned strip, the continuation wins while it's pending.
    question_mark(&mut f);
    f.key(Key::G);
    assert!(f.drew("first") && !f.drew("down"));
    f.key(Key::G);
    assert!(f.drew("down"), "the pinned strip is back");
}

/// The strip floats over the app rather than taking its bottom: the app lays
/// out in the same room whether a chord's strip or the pinned one shows.
#[tokio::test]
async fn the_strip_never_moves_the_app() {
    let mut f = fixture();
    let area = |f: &Fixture| f.state.borrow().area.expect("the app rendered");
    let hidden = area(&f);

    f.key(Key::G);
    assert!(f.drew("first"), "the chord's strip shows");
    assert_eq!(area(&f), hidden);

    f.key(Key::G);
    assert!(!f.drew("first"));
    assert_eq!(area(&f), hidden);

    question_mark(&mut f);
    assert!(f.drew("down"), "the pinned strip shows");
    assert_eq!(area(&f), hidden);
}

#[tokio::test]
async fn an_unavailable_tool_is_left_out() {
    let mut f = fixture();
    f.state.borrow_mut().gated_available = false;
    question_mark(&mut f);
    assert!(!f.drew("gated"));
    assert!(f.drew("down"));

    f.state.borrow_mut().gated_available = true;
    f.frame(Vec::new());
    assert!(f.drew("gated"));
}

/// The presses a keycap stands for: `gg` is two, a capital or a shifted symbol
/// is Shift plus the key, `^f` is Ctrl+F, and the named keycaps are their keys.
fn keycap_presses(cap: &str) -> Vec<KeyboardShortcut> {
    match cap {
        "\u{21B5}" => return vec![KeyboardShortcut::new(Modifiers::NONE, Key::Enter)],
        "esc" => return vec![KeyboardShortcut::new(Modifiers::NONE, Key::Escape)],
        "space" => return vec![KeyboardShortcut::new(Modifiers::NONE, Key::Space)],
        _ => {}
    }
    if let Some(letter) = cap.strip_prefix('^') {
        let key = Key::from_name(letter).unwrap_or_else(|| panic!("keycap {cap:?}"));
        return vec![KeyboardShortcut::new(
            Modifiers::CTRL | Modifiers::COMMAND,
            key,
        )];
    }
    cap.chars()
        .map(|c| {
            let key = Key::from_name(&c.to_string())
                .unwrap_or_else(|| panic!("keycap {cap:?}: no key for {c:?}"));
            let shifted = c.is_ascii_uppercase() || "?!:|+{}".contains(c);
            let modifiers = if shifted {
                Modifiers::SHIFT
            } else {
                Modifiers::NONE
            };
            KeyboardShortcut::new(modifiers, key)
        })
        .collect()
}

/// Every keycap the pinned strip shows, pressed through chrome on a fresh
/// fixture, runs the tool its label names: the strip can't advertise a key the
/// keymap would send elsewhere, and its keycaps are spelled so they can be
/// typed.
#[tokio::test]
async fn every_hint_does_what_it_says() {
    let mut f = fixture();
    question_mark(&mut f);
    let hints = shown(&mut f).expect("pinned");
    assert_eq!(hints.len(), LABEL_TOOLS.len(), "{hints:?}");

    for ShownHint { keycap: cap, label } in hints {
        let (_, tool) = LABEL_TOOLS
            .iter()
            .find(|(l, _)| *l == label)
            .unwrap_or_else(|| panic!("no tool for label {label:?}"));

        let mut f = fixture();
        question_mark(&mut f);
        for press in keycap_presses(&cap) {
            f.press(press.modifiers, press.logical_key, None);
        }

        match *tool {
            // The chrome's own: both leave the strip unpinned.
            TOGGLE_KEY_HINTS | HIDE_KEY_HINTS => {
                assert!(!f.chrome.key_hints_pinned(), "{cap:?} ({label})");
            }
            tool => assert_eq!(f.calls(), [tool], "{cap:?} ({label})"),
        }
    }
}
