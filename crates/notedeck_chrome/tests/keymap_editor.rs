//! Editing the account's keymap inside notedeck, through real chrome frames:
//! `chrome_edit_keymap` opens Notebook's keymap editor on the stored
//! overrides, and saving from it merges, writes and applies them, or shows
//! why not.
//!
//! Notebook is in front throughout and every key is a global binding:
//! switching apps slides egui_nav, which trips egui's debug layer assert.

#![cfg(feature = "notebook")]

mod common;

use std::time::Duration;

use common::Fixture;
use egui::{Event, Key, Modifiers};
use nostrdb::{Ndb, SubscriptionStream};
use nostrdb_net::FullKeypair;
use notedeck_chrome::keymap::{user, BindAction, GLOBAL_SCOPE};
use notedeck_chrome::{NotedeckApp, KEYMAP_TEMPLATE};

/// How long a test waits for nostrdb to ingest a keymap note.
const INGEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The fixture's starting keymap: `e` opens the keymap editor anywhere.
const KEYMAP: &str = r#"
[[bind]]
scope = "global"
keys = "e"
tool = "chrome_edit_keymap"
"#;

/// A user keymap binding `e` to the editor as [`KEYMAP`] does: one is merged
/// over the built-in keymap, not the fixture's, so it brings its own.
const E_EDITS: &str = "[[bind]]\nscope = \"global\"\nkeys = \"e\"\ntool = \"chrome_edit_keymap\"\n";

/// [`E_EDITS`], and `x` opens the editor too.
const X_EDITS: &str =
    "[[bind]]\nscope = \"global\"\nkeys = \"e\"\ntool = \"chrome_edit_keymap\"\n\n\
                       [[bind]]\nscope = \"global\"\nkeys = \"x\"\ntool = \"chrome_edit_keymap\"\n";

/// The fixture's nostrdb.
fn ndb(f: &mut Fixture) -> Ndb {
    f.notedeck.app_context().ndb.clone()
}

/// A signed-in fixture with Notebook in front, its account's keypair, and a
/// stream over the account's keymap notes, subscribed before anything is
/// written.
struct SignedIn {
    fixture: Fixture,
    kp: FullKeypair,
    stream: SubscriptionStream,
}

/// A fresh [`SignedIn`] account.
fn signed_in() -> SignedIn {
    let kp = FullKeypair::generate();
    let mut f = Fixture::signed_in_behind(
        KEYMAP,
        &kp.secret_key.to_secret_hex(),
        [NotedeckApp::Notebook(Box::default())],
    );
    let ndb = ndb(&mut f);
    let sub = ndb
        .subscribe(&[user::user_keymap_filter(&kp.pubkey)])
        .expect("subscribe");
    let stream = SubscriptionStream::new(ndb, sub).notes_per_await(64);
    SignedIn {
        fixture: f,
        kp,
        stream,
    }
}

/// Save `toml` through the chrome and wait for it to land in nostrdb.
async fn saved(f: &mut Fixture, stream: &mut SubscriptionStream, toml: &str) {
    let mut ctx = f.notedeck.app_context();
    f.chrome.save_user_keymap(&mut ctx, toml).expect("saved");
    stream
        .wait_for_notes(1, INGEST_TIMEOUT)
        .await
        .expect("the keymap note ingested");
}

/// Press `key` to open the editor, and let it take focus.
fn open_editor(f: &mut Fixture, key: Key) {
    f.key(key);
    f.frame(Vec::new());
}

/// Replace the editor's text with `toml`, as select-all and a paste would.
fn retype(f: &mut Fixture, toml: &str) {
    f.frame(vec![
        Event::Key {
            key: Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::COMMAND,
        },
        Event::Paste(toml.to_owned()),
    ]);
    // The header, drawn above the text, shows the change a frame later.
    f.frame(Vec::new());
}

/// Save from the editor with Ctrl+S, and run the frame after it.
fn save_in_editor(f: &mut Fixture) {
    f.press(Modifiers::COMMAND, Key::S, None);
    f.frame(Vec::new());
}

/// Close the editor with Esc, which egui has already taken the text
/// field's focus for by the time the editor reads it.
fn close_editor(f: &mut Fixture) {
    f.key(Key::Escape);
}

/// Whether the keymap editor is showing, by the line under its title.
fn editor_showing(f: &Fixture) -> bool {
    drew_part(f, "merged over the built-in keymap")
}

/// Whether some text the last frame drew contains `needle`.
fn drew_part(f: &Fixture, needle: &str) -> bool {
    f.drawn.iter().any(|text| text.contains(needle))
}

#[tokio::test]
async fn the_editor_opens_on_the_saved_keymap_and_a_save_rebinds_live() {
    let SignedIn {
        fixture: mut f,
        kp,
        mut stream,
    } = signed_in();
    saved(&mut f, &mut stream, E_EDITS).await;

    open_editor(&mut f, Key::E);
    assert!(editor_showing(&f), "the editor is showing: {:?}", f.drawn);
    assert!(f.drew(E_EDITS), "seeded with the saved keymap");

    retype(&mut f, X_EDITS);
    assert!(f.drew(X_EDITS), "the edit landed: {:?}", f.drawn);
    assert!(f.drew("●"), "it is unsaved");
    save_in_editor(&mut f);
    stream
        .wait_for_notes(1, INGEST_TIMEOUT)
        .await
        .expect("the edited keymap ingested");
    assert_eq!(
        user::load_user_keymap(&ndb(&mut f), &kp.pubkey).as_deref(),
        Some(X_EDITS),
        "a new revision is stored"
    );
    assert!(!f.drew("●"), "the editor is clean after the save");

    close_editor(&mut f);
    f.frame(Vec::new());
    assert!(!editor_showing(&f), "closed: {:?}", f.drawn);
    assert!(!f.drew("Settings"), "the drawer stayed shut: {:?}", f.drawn);
    open_editor(&mut f, Key::X);
    assert!(
        editor_showing(&f),
        "x is bound with no restart: {:?}",
        f.drawn
    );
}

#[tokio::test]
async fn a_keymap_that_does_not_merge_shows_why_and_changes_nothing() {
    let SignedIn {
        fixture: mut f,
        kp,
        mut stream,
    } = signed_in();
    saved(&mut f, &mut stream, X_EDITS).await;

    open_editor(&mut f, Key::E);
    let unknown_tool = X_EDITS.replace(
        "\"x\"\ntool = \"chrome_edit_keymap\"",
        "\"x\"\ntool = \"chrome_edit_keymaps\"",
    );
    retype(&mut f, &unknown_tool);
    save_in_editor(&mut f);
    assert!(
        drew_part(&f, "no app has a tool 'chrome_edit_keymaps'"),
        "the reason is shown in the editor: {:?}",
        f.drawn
    );
    assert!(f.drew("●"), "the change is still unsaved");

    assert_eq!(
        user::load_user_keymap(&ndb(&mut f), &kp.pubkey).as_deref(),
        Some(X_EDITS),
        "the stored note is untouched"
    );
    let bound = |f: &Fixture, tool: &str| {
        f.chrome
            .keymap()
            .bindings(GLOBAL_SCOPE)
            .iter()
            .any(|b| matches!(&b.action, BindAction::Tool(call) if call.name == tool))
    };
    assert!(
        bound(&f, "chrome_edit_keymap"),
        "the keymap in use is untouched"
    );
    assert!(!bound(&f, "chrome_edit_keymaps"));
}

#[tokio::test]
async fn an_account_with_no_keymap_gets_the_template() {
    let SignedIn { fixture: mut f, .. } = signed_in();
    open_editor(&mut f, Key::E);
    assert!(f.drew(KEYMAP_TEMPLATE), "{:?}", f.drawn);
}
