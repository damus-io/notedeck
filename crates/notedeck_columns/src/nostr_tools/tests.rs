//! The nostr tools over a small seeded world: alice (the selected account)
//! follows bob; bob posts a root note, carol replies to it, alice replies to
//! carol; carol also posts on her own.

use nostrdb::{IngestMetadata, Ndb, NoteBuilder, SubscriptionStream};
use nostrdb_net::{ClientMessage, FullKeypair};
use notedeck::{RegisteredTool, ToolOutcome};
use notedeck_testing::TestHost;
use serde_json::{json, Value};

use super::*;

/// When the seeded world starts.
const T0: u64 = 1_700_000_000;

/// The seeded world: who's who and the ids of the notes each wrote.
struct World {
    host: TestHost,
    alice: FullKeypair,
    bob: FullKeypair,
    carol: FullKeypair,
    /// Bob's root post, tagged #nostr, at T0.
    root: [u8; 32],
    /// Carol's reply to the root, at T0+10.
    carol_reply: [u8; 32],
    /// Alice's reply to carol's reply, at T0+20.
    alice_reply: [u8; 32],
    /// Bob's second post, at T0+30.
    bob_later: [u8; 32],
    /// Carol's own post, at T0+5.
    carol_post: [u8; 32],
}

/// A note `kp` signs: `kind`, `content` at `created_at`, with `tags`, each
/// tag a list of strings.
fn signed(kp: &FullKeypair, kind: u32, content: &str, created_at: u64, tags: &[&[&str]]) -> String {
    let mut builder = NoteBuilder::new()
        .kind(kind)
        .content(content)
        .created_at(created_at);
    for tag in tags {
        builder = builder.start_tag();
        for part in *tag {
            builder = builder.tag_str(part);
        }
    }
    let secret = kp.secret_key.secret_bytes();
    let note = builder.sign(&secret).build().expect("note");
    ClientMessage::event(&note)
        .expect("event")
        .to_json()
        .expect("json")
}

/// The id of a signed frame's note.
fn id_of(frame: &str) -> [u8; 32] {
    let value: Value = serde_json::from_str(frame).expect("frame");
    let hex = value[1]["id"].as_str().expect("id");
    hex::decode(hex).expect("hex").try_into().expect("32 bytes")
}

/// Seed the world into a fresh host signed in as alice, waiting until every
/// note has landed.
fn world() -> World {
    let alice = FullKeypair::generate();
    let bob = FullKeypair::generate();
    let carol = FullKeypair::generate();
    let mut host = TestHost::with_account(&alice);

    let root = signed(&bob, 1, "gm #nostr", T0, &[&["t", "nostr"]]);
    let root_hex = hex::encode(id_of(&root));
    let carol_reply = signed(
        &carol,
        1,
        "gm bob",
        T0 + 10,
        &[&["e", &root_hex, "", "root"], &["p", &bob.pubkey.hex()]],
    );
    let carol_reply_hex = hex::encode(id_of(&carol_reply));
    let alice_reply = signed(
        &alice,
        1,
        "gm both",
        T0 + 20,
        &[
            &["e", &root_hex, "", "root"],
            &["e", &carol_reply_hex, "", "reply"],
        ],
    );
    let bob_later = signed(&bob, 1, "second thought", T0 + 30, &[]);
    let carol_post = signed(&carol, 1, "carol here", T0 + 5, &[]);
    let frames = vec![
        signed(
            &bob,
            0,
            r#"{"name":"bob","display_name":"Bob B","about":"builds things","nip05":"bob@example.com"}"#,
            T0,
            &[],
        ),
        signed(&alice, 3, "", T0, &[&["p", &bob.pubkey.hex()]]),
        signed(
            &bob,
            3,
            "",
            T0,
            &[&["p", &alice.pubkey.hex()], &["p", &carol.pubkey.hex()]],
        ),
        root.clone(),
        carol_reply.clone(),
        alice_reply.clone(),
        bob_later.clone(),
        carol_post.clone(),
    ];

    {
        let ctx = host.app_context();
        let ndb: &Ndb = ctx.ndb;
        // Subscribe before ingesting, so no note lands unseen.
        let authors = [
            alice.pubkey.bytes(),
            bob.pubkey.bytes(),
            carol.pubkey.bytes(),
        ];
        let sub = ndb
            .subscribe(&[Filter::new().authors(authors).build()])
            .expect("subscribe");
        let mut stream = SubscriptionStream::new(ndb.clone(), sub);
        for frame in &frames {
            ndb.process_event_with(frame, IngestMetadata::new().client(true))
                .expect("ingest");
        }
        notedeck_testing::await_notes(&mut stream, frames.len());
    }

    World {
        host,
        root: id_of(&root),
        carol_reply: id_of(&carol_reply),
        alice_reply: id_of(&alice_reply),
        bob_later: id_of(&bob_later),
        carol_post: id_of(&carol_post),
        alice,
        bob,
        carol,
    }
}

impl World {
    /// Call the tool named `name` with `args`, as a backend would.
    fn call(&mut self, name: &str, args: Value) -> Result<Value, String> {
        let tool = tools()
            .into_iter()
            .find(|t| t.name() == name)
            .expect("tool");
        let mut ctx = self.host.app_context();
        let mut cx = ctx.tool_context();
        match RegisteredTool::call(&tool, &mut cx, &args) {
            ToolOutcome::Data(value) => Ok(value),
            ToolOutcome::Error(err) => Err(err),
            ToolOutcome::Intent(_) => panic!("a data tool raised an intent"),
        }
    }
}

/// The note ids of an answer's `notes`, hex.
fn note_ids(out: &Value) -> Vec<String> {
    out["notes"]
        .as_array()
        .expect("notes")
        .iter()
        .map(|n| n["note_id"].as_str().expect("note_id").to_owned())
        .collect()
}

/// A profile answers the kind 0's fields, how many they follow, and
/// whether the account follows them; an npub with a `nostr:` prefix names
/// them.
#[test]
fn get_profile_reads_a_profile_and_the_follow_graph() {
    let mut w = world();
    let npub = w.bob.pubkey.npub().unwrap();
    let out = w
        .call(
            "nostr_get_profile",
            json!({ "who": format!("nostr:{npub}") }),
        )
        .unwrap();
    assert_eq!(out["found"], json!(true));
    assert_eq!(out["pubkey"], json!(w.bob.pubkey.hex()));
    assert_eq!(out["name"], json!("bob"));
    assert_eq!(out["display_name"], json!("Bob B"));
    assert_eq!(out["nip05"], json!("bob@example.com"));
    assert_eq!(out["following"], json!(2));
    assert_eq!(out["followed_by_you"], json!(true));
    assert_eq!(out["is_you"], json!(false));
}

/// With no `who`, the account's own profile; one this notedeck never saw
/// answers `found: false` rather than failing.
#[test]
fn get_profile_defaults_to_the_account_and_answers_an_unseen_one() {
    let mut w = world();
    let out = w.call("nostr_get_profile", json!({})).unwrap();
    assert_eq!(out["pubkey"], json!(w.alice.pubkey.hex()));
    assert_eq!(out["is_you"], json!(true));
    assert_eq!(out["found"], json!(false));
    assert_eq!(out["following"], json!(1));

    let stranger = FullKeypair::generate().pubkey.hex();
    let out = w
        .call("nostr_get_profile", json!({ "who": stranger }))
        .unwrap();
    assert_eq!(out["found"], json!(false));
    assert_eq!(out["following"], Value::Null);
    assert_eq!(out["followed_by_you"], json!(false));

    let err = w
        .call("nostr_get_profile", json!({ "who": "bob" }))
        .unwrap_err();
    assert!(err.contains("not a pubkey"), "{err}");
}

/// A thread from its deepest note answers the root, the chain and the
/// reply, oldest first, each parent named by `reply_to`; a note1 names a
/// note as well as hex does.
#[test]
fn load_thread_walks_up_to_the_root_and_gathers_the_replies() {
    let mut w = world();
    let deepest = nostrdb_net::NoteId::new(w.alice_reply).to_bech().unwrap();
    let out = w
        .call("nostr_load_thread", json!({ "note": deepest }))
        .unwrap();
    let root = hex::encode(w.root);
    let carol_reply = hex::encode(w.carol_reply);
    let alice_reply = hex::encode(w.alice_reply);
    assert_eq!(out["root"], json!(root));
    assert_eq!(out["focus"], json!(alice_reply));
    assert_eq!(note_ids(&out), vec![root, carol_reply.clone(), alice_reply]);
    assert_eq!(out["notes"][2]["reply_to"], json!(carol_reply));
    assert_eq!(out["missing"], json!([]));

    // From the root, the same thread.
    let out = w
        .call("nostr_load_thread", json!({ "note": hex::encode(w.root) }))
        .unwrap();
    assert_eq!(note_ids(&out).len(), 3);
}

/// A reply whose parent this notedeck never saw answers what it has and
/// names the missing ids; a note it never saw is an error.
#[test]
fn load_thread_names_what_it_doesnt_have() {
    let mut w = world();
    let ghost = hex::encode([7u8; 32]);
    let orphan = signed(
        &w.carol,
        1,
        "replying into the void",
        T0 + 40,
        &[&["e", &ghost, "", "root"]],
    );
    {
        let ctx = w.host.app_context();
        let sub = ctx
            .ndb
            .subscribe(&[Filter::new().ids([&id_of(&orphan)]).build()])
            .expect("subscribe");
        let mut stream = SubscriptionStream::new(ctx.ndb.clone(), sub);
        ctx.ndb
            .process_event_with(&orphan, IngestMetadata::new().client(true))
            .expect("ingest");
        notedeck_testing::await_notes(&mut stream, 1);
    }
    let out = w
        .call(
            "nostr_load_thread",
            json!({ "note": hex::encode(id_of(&orphan)) }),
        )
        .unwrap();
    assert_eq!(out["root"], json!(ghost));
    assert_eq!(out["missing"], json!([ghost]));
    assert_eq!(note_ids(&out), vec![hex::encode(id_of(&orphan))]);

    let err = w
        .call(
            "nostr_load_thread",
            json!({ "note": hex::encode([9u8; 32]) }),
        )
        .unwrap_err();
    assert!(err.contains("isn't in the local db"), "{err}");
}

/// The home timeline is the followed accounts' notes, newest first, with
/// replies left out.
#[test]
fn read_timeline_home_is_the_follows_newest_first() {
    let mut w = world();
    let out = w.call("nostr_read_timeline", json!({})).unwrap();
    assert_eq!(out["source"], json!("home"));
    assert_eq!(
        note_ids(&out),
        vec![hex::encode(w.bob_later), hex::encode(w.root)]
    );
}

/// An author's timeline keeps replies when asked; a hashtag's is the notes
/// tagged with it; `limit` and `until` page.
#[test]
fn read_timeline_by_author_and_hashtag() {
    let mut w = world();
    let carol = w.carol.pubkey.npub().unwrap();
    let out = w
        .call("nostr_read_timeline", json!({ "author": carol }))
        .unwrap();
    assert_eq!(note_ids(&out), vec![hex::encode(w.carol_post)]);

    let out = w
        .call(
            "nostr_read_timeline",
            json!({ "author": carol, "replies": true }),
        )
        .unwrap();
    assert_eq!(
        note_ids(&out),
        vec![hex::encode(w.carol_reply), hex::encode(w.carol_post)]
    );

    let out = w
        .call("nostr_read_timeline", json!({ "hashtag": "#Nostr" }))
        .unwrap();
    assert_eq!(out["source"], json!("hashtag"));
    assert_eq!(note_ids(&out), vec![hex::encode(w.root)]);

    let out = w
        .call(
            "nostr_read_timeline",
            json!({ "limit": 1, "until": T0 + 29 }),
        )
        .unwrap();
    assert_eq!(note_ids(&out), vec![hex::encode(w.root)]);

    let err = w
        .call(
            "nostr_read_timeline",
            json!({ "author": carol, "hashtag": "nostr" }),
        )
        .unwrap_err();
    assert!(err.contains("not both"), "{err}");
}

/// Every tool is a read a model can call: a data tool under `nd nostr`,
/// read-only (so Observe in the approval table), answering once.
#[test]
fn the_tools_are_model_callable_reads_under_nd_nostr() {
    let tools = tools();
    assert_eq!(tools.len(), 3);
    for tool in &tools {
        let spec = tool.spec();
        assert_eq!(
            spec.command_path().first(),
            Some(&"nostr"),
            "{}",
            spec.name()
        );
        assert_eq!(spec.kind(), notedeck::ToolKind::Data);
        assert!(spec.stream_mode().is_none());
        assert_eq!(spec.capability_class(), notedeck::tool::Capability::Observe);
    }
}
