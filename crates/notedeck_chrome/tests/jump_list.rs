//! Vim's jump list through real chrome frames: a binding marked `jump`
//! records where the fake app was, Ctrl-O goes back there and Ctrl-I
//! forward again, and with nowhere to go both keys fall through to the app.

mod common;

use common::Fixture;
use egui::{Key, Modifiers};
use notedeck_chrome::keymap::DEFAULT_KEYMAP;

/// `j` steps, `g g` jumps to the top, and the chrome's two jump keys.
const KEYMAP: &str = r#"
[[bind]]
scope = "fake"
keys  = "j"
tool  = "fake_down"

[[bind]]
scope = "fake"
keys  = "g g"
tool  = "fake_first"
jump  = true

[[bind]]
scope = "global"
keys  = "<C-o>"
tool  = "chrome_jump_back"

[[bind]]
scope = "global"
keys  = "<C-i>"
tool  = "chrome_jump_forward"
"#;

fn ctrl(fixture: &mut Fixture, key: Key) {
    fixture.press(Modifiers::CTRL, key, None);
}

#[test]
fn ctrl_o_returns_from_a_jump_and_ctrl_i_goes_again() {
    let mut f = Fixture::new(KEYMAP);
    for _ in 0..3 {
        f.key(Key::J);
    }
    assert_eq!(f.location(), 3);

    f.key(Key::G);
    f.key(Key::G);
    assert_eq!(f.location(), 0, "g g went to the top");

    ctrl(&mut f, Key::O);
    assert_eq!(f.location(), 3, "Ctrl-O is back where g g left");
    ctrl(&mut f, Key::I);
    assert_eq!(f.location(), 0, "Ctrl-I is at the top again");
    assert!(
        !f.seen().contains(&Key::O) && !f.seen().contains(&Key::I),
        "the chrome took both"
    );
}

/// `j` is not a jump: with nothing recorded, Ctrl-O has nowhere to go and
/// reaches the app.
#[test]
fn with_no_jump_the_keys_fall_through() {
    let mut f = Fixture::new(KEYMAP);
    f.key(Key::J);
    f.key(Key::J);
    ctrl(&mut f, Key::O);
    ctrl(&mut f, Key::I);
    assert_eq!(f.location(), 2, "nothing moved the view");
    assert!(f.seen().contains(&Key::O));
    assert!(f.seen().contains(&Key::I));
}

/// After going back, a new jump forgets what was ahead.
#[test]
fn a_new_jump_after_going_back_drops_the_forward_part() {
    let mut f = Fixture::new(KEYMAP);
    f.key(Key::J);
    f.key(Key::G);
    f.key(Key::G); // jump from 1
    ctrl(&mut f, Key::O); // back at 1, 0 ahead
    assert_eq!(f.location(), 1);

    f.key(Key::J);
    f.key(Key::G);
    f.key(Key::G); // jump from 2: 0 ahead is forgotten
    ctrl(&mut f, Key::I);
    assert_eq!(f.location(), 0, "Ctrl-I had nowhere to go");
    assert!(f.seen().contains(&Key::I));
    ctrl(&mut f, Key::O);
    assert_eq!(f.location(), 2);
    ctrl(&mut f, Key::O);
    assert_eq!(f.location(), 1);
}

/// The shipped keymap binds both keys globally, so every app gets them.
#[test]
fn the_default_keymap_binds_ctrl_o_and_ctrl_i() {
    let keymap = notedeck_chrome::keymap::parse(DEFAULT_KEYMAP).expect("default keymap");
    let tools: Vec<&str> = keymap
        .bindings("global")
        .iter()
        .filter_map(|b| match &b.action {
            notedeck_chrome::keymap::BindAction::Tool(call) => Some(call.name.as_str()),
            _ => None,
        })
        .collect();
    assert!(tools.contains(&notedeck_chrome::JUMP_BACK));
    assert!(tools.contains(&notedeck_chrome::JUMP_FORWARD));
}

/// Ctrl-O / Ctrl-I are bound twice in `global`: the jump list first, then
/// the nav stack's back / forward, so the keys reach the app's history once
/// the jump list has nowhere to go (each is offered only while it does).
#[test]
fn the_default_keymap_falls_ctrl_o_and_ctrl_i_through_to_nav() {
    let keymap = notedeck_chrome::keymap::parse(DEFAULT_KEYMAP).expect("default keymap");
    let tools_on = |key: &str| -> Vec<String> {
        keymap
            .bindings("global")
            .iter()
            .filter(|b| b.keys.to_string() == key)
            .filter_map(|b| match &b.action {
                notedeck_chrome::keymap::BindAction::Tool(call) => Some(call.name.clone()),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        tools_on("<C-o>"),
        vec![notedeck_chrome::JUMP_BACK, notedeck_chrome::NAV_BACK]
    );
    assert_eq!(
        tools_on("<C-i>"),
        vec![notedeck_chrome::JUMP_FORWARD, notedeck_chrome::NAV_FORWARD]
    );
}
