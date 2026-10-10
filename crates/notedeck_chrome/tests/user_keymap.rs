//! The account's keymap overrides through real chrome frames: saved as a
//! private nostr note, merged over the built-in keymap, and picked up live
//! when a revision lands, with no restart.

mod common;

use std::time::Duration;

use common::Fixture;
use egui::Key;
use nostrdb::{Ndb, SubscriptionStream};
use nostrdb_net::FullKeypair;
use notedeck_chrome::keymap::user;

/// How long a test waits for nostrdb to ingest a keymap note.
const INGEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The keymap the fixture starts with, standing in for the defaults: `j`
/// runs `fake_down`.
const KEYMAP: &str = r#"
[[bind]]
scope = "fake"
keys = "j"
tool = "fake_down"
"#;

/// A user keymap binding `z` in the fake app's scope.
const Z_FIRST: &str = r#"
[[bind]]
scope = "fake"
keys = "z"
tool = "fake_first"
"#;

/// The fixture's nostrdb.
fn ndb(f: &mut Fixture) -> Ndb {
    f.notedeck.app_context().ndb.clone()
}

/// Save `toml` as the account's keymap through the chrome.
fn save(f: &mut Fixture, toml: &str) -> Result<(), String> {
    let mut ctx = f.notedeck.app_context();
    f.chrome.save_user_keymap(&mut ctx, toml)
}

/// A signed-in fixture, its account's keypair, and a stream over the
/// account's keymap notes, subscribed before anything is written.
struct SignedIn {
    fixture: Fixture,
    kp: FullKeypair,
    stream: SubscriptionStream,
}

/// A fresh [`SignedIn`] account.
fn signed_in() -> SignedIn {
    let kp = FullKeypair::generate();
    let mut f = Fixture::signed_in(KEYMAP, &kp.secret_key.to_secret_hex());
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

/// Write `toml` as `kp`'s keymap the way another device's would arrive:
/// straight into nostrdb, not through the chrome.
async fn write_elsewhere(
    f: &mut Fixture,
    kp: &FullKeypair,
    stream: &mut SubscriptionStream,
    toml: &str,
) {
    let ndb = ndb(f);
    let secret = kp.secret_key.secret_bytes();
    user::write_user_keymap(&ndb, &kp.pubkey, &secret, toml, 0).expect("write");
    stream
        .wait_for_notes(1, INGEST_TIMEOUT)
        .await
        .expect("the keymap note ingested");
}

#[tokio::test]
async fn a_keymap_saved_elsewhere_takes_effect_without_a_restart() {
    let SignedIn {
        fixture: mut f,
        kp,
        mut stream,
    } = signed_in();
    f.key(Key::J);
    assert_eq!(f.calls(), ["fake_down"], "the starting keymap");

    write_elsewhere(&mut f, &kp, &mut stream, Z_FIRST).await;
    f.frame(Vec::new());

    f.key(Key::Z);
    assert_eq!(f.calls(), ["fake_down", "fake_first"], "z is bound now");
    assert_eq!(f.chrome.keymap_error(), None);

    // Merged over the built-in keymap, which doesn't bind the fake app's j.
    f.key(Key::J);
    assert_eq!(f.calls(), ["fake_down", "fake_first"]);
    assert_eq!(f.seen(), [Key::J], "an unbound j reaches the app");
}

#[tokio::test]
async fn saving_a_keymap_uses_it_at_once_and_a_resave_supersedes_it() {
    let SignedIn {
        fixture: mut f,
        kp,
        mut stream,
    } = signed_in();
    save(&mut f, Z_FIRST).expect("a good keymap saves");
    f.key(Key::Z);
    assert_eq!(f.calls(), ["fake_first"]);

    // Within the same second: stamped past the first, so it wins.
    let z_down = Z_FIRST.replace("fake_first", "fake_down");
    save(&mut f, &z_down).expect("a good keymap saves");
    stream
        .wait_for_notes(2, INGEST_TIMEOUT)
        .await
        .expect("both revisions ingested");
    // The subscription fires for both; reading back lands on the newest.
    f.frame(Vec::new());
    f.key(Key::Z);
    assert_eq!(f.calls(), ["fake_first", "fake_down"]);

    let ndb = ndb(&mut f);
    assert_eq!(
        user::load_user_keymap(&ndb, &kp.pubkey).as_deref(),
        Some(z_down.as_str())
    );
}

#[tokio::test]
async fn a_keymap_that_does_not_merge_is_refused_and_nothing_is_written() {
    let SignedIn {
        fixture: mut f,
        kp,
        stream: _stream,
    } = signed_in();
    let unknown_tool = Z_FIRST.replace("fake_first", "fake_frist");
    let err = save(&mut f, &unknown_tool).expect_err("an unknown tool is refused");
    assert!(err.contains("no app has a tool 'fake_frist'"), "{err}");

    save(&mut f, "[[bind]\n").expect_err("bad TOML is refused");

    let ndb = ndb(&mut f);
    assert_eq!(user::load_user_keymap(&ndb, &kp.pubkey), None);
    f.key(Key::J);
    assert_eq!(f.calls(), ["fake_down"], "the keymap in use is untouched");
}

#[tokio::test]
async fn a_broken_stored_keymap_keeps_the_defaults_and_says_why() {
    let SignedIn {
        fixture: mut f,
        kp,
        mut stream,
    } = signed_in();
    write_elsewhere(&mut f, &kp, &mut stream, Z_FIRST).await;
    f.frame(Vec::new());
    f.key(Key::Z);
    assert_eq!(f.calls(), ["fake_first"]);

    // A revision naming a tool no app has, as a device with another app
    // roster might write: none of it applies, and the defaults come back.
    let broken = format!("{Z_FIRST}\n[[bind]]\nscope = \"fake\"\nkeys = \"y\"\ntool = \"nope\"\n");
    write_elsewhere(&mut f, &kp, &mut stream, &broken).await;
    f.frame(Vec::new());

    let err = f.chrome.keymap_error().expect("the error is kept");
    assert!(err.contains("no app has a tool 'nope'"), "{err}");
    f.key(Key::Z);
    assert_eq!(f.calls(), ["fake_first"], "z isn't bound in the defaults");
    assert_eq!(f.seen(), [Key::Z]);
}

#[tokio::test]
async fn a_watch_only_account_cannot_save_a_keymap() {
    let mut f = Fixture::new(KEYMAP);
    let err = save(&mut f, Z_FIRST).expect_err("no secret to sign with");
    assert!(err.contains("watch-only"), "{err}");
}
