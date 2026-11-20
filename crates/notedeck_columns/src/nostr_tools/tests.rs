//! The nostr tools over a small seeded world: alice (the selected account)
//! follows bob; bob posts a root note, carol replies to it, alice replies to
//! carol; carol also posts on her own.

use nostrdb::{IngestMetadata, Ndb, NoteBuilder, SubscriptionStream};
use nostrdb_net::{ClientMessage, FullKeypair};
use notedeck::{RegisteredTool, Started, ToolOutcome};
use notedeck_testing::TestHost;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

use super::*;

/// When the seeded world starts.
const T0: u64 = 1_700_000_000;

/// How long a test's reads wait on a fetch the test never answers: no
/// relay runs here, so every miss costs this much.
const MISS_BOUND: Duration = Duration::from_millis(50);

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
    world_of(
        FullKeypair::generate(),
        FullKeypair::generate(),
        FullKeypair::generate(),
    )
}

/// [`world`] with these keys, so two worlds can hold the same notes.
fn world_of(alice: FullKeypair, bob: FullKeypair, carol: FullKeypair) -> World {
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
        let tool = tools_within(MISS_BOUND)
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

/// The reads are what a model can call unasked: data tools under `nd
/// nostr`, read-only (so Observe in the approval table), answering once.
/// The writes beside them declare ActAsUser.
#[test]
fn the_reads_are_observe_and_the_writes_act_as_the_user() {
    let tools = tools();
    let names: Vec<_> = tools.iter().map(|t| t.name()).collect();
    assert_eq!(
        names,
        [
            "nostr_get_profile",
            "nostr_load_thread",
            "nostr_read_timeline",
            "nostr_post",
            "nostr_reply",
            "nostr_react",
            "nostr_follow",
            "nostr_unfollow",
        ]
    );
    for (i, tool) in tools.iter().enumerate() {
        let spec = tool.spec();
        assert_eq!(
            spec.command_path().first(),
            Some(&"nostr"),
            "{}",
            spec.name()
        );
        assert_eq!(spec.kind(), notedeck::ToolKind::Data);
        assert!(spec.stream_mode().is_none());
        let class = if i < 3 {
            notedeck::tool::Capability::Observe
        } else {
            notedeck::tool::Capability::ActAsUser
        };
        assert_eq!(spec.capability_class(), class, "{}", spec.name());
    }
}

/// A spec naming the `nostr` app is offered every nostr tool. The reads
/// (Observe) and opens (Navigate) run unasked; the writes and the zap
/// (ActAsUser) ask every time, and Plan refuses them. This is the catalog
/// built from the specs Columns registers, not from stand-ins.
#[test]
fn a_nostr_toolset_offers_reads_and_opens_unasked_and_gates_the_actions() {
    use agentium_core::agent_spec::{AgentSpec, Approval, Catalog};
    use notedeck::tool::Capability;

    let spec: AgentSpec = serde_json::from_value(json!({
        "slug": "nostr",
        "model": { "provider": "anthropic", "model": "claude-sonnet-5-5" },
        "prompt": "Look around nostr.",
        "toolsets": [{ "kind": "notedeck", "apps": ["nostr"] }],
        "budget": { "max_steps": 10 }
    }))
    .expect("spec");
    // Filed under `nostr`, as the chrome files Columns' tools from its
    // `tool_app`.
    let data: Vec<ToolSpec> = tools()
        .into_iter()
        .map(|t| t.in_app(TOOL_APP).spec().clone())
        .collect();
    let mut ui = open::specs();
    ui.extend(act::ui_specs());
    let ui: Vec<ToolSpec> = ui.into_iter().map(|s| s.in_app(TOOL_APP)).collect();
    let catalog = Catalog::build(&spec, data.iter().chain(&ui)).expect("catalog");

    let offered: Vec<_> = catalog.tools().iter().map(|t| t.spec.name()).collect();
    let unasked = [
        "nostr_get_profile",
        "nostr_load_thread",
        "nostr_read_timeline",
        open::OPEN_PROFILE,
        open::OPEN_THREAD,
        open::OPEN_HASHTAG,
    ];
    let gated = [
        "nostr_post",
        "nostr_reply",
        "nostr_react",
        "nostr_follow",
        "nostr_unfollow",
        act::ZAP,
    ];
    assert_eq!(offered.len(), unasked.len() + gated.len(), "{offered:?}");
    for name in unasked {
        assert_eq!(
            catalog.get(name).unwrap().approval,
            Approval::Auto,
            "{name}"
        );
    }
    for name in gated {
        let tool = catalog.get(name).unwrap();
        assert_eq!(tool.class, Capability::ActAsUser, "{name}");
        assert_eq!(tool.approval, Approval::Gated, "{name}");
    }
    for name in [open::OPEN_PROFILE, open::OPEN_THREAD, open::OPEN_HASHTAG] {
        assert_eq!(catalog.get(name).unwrap().class, Capability::Navigate);
    }
}

// ---------------------------------------------------------------------------
// the writes
// ---------------------------------------------------------------------------

/// What a write answered and the note it wrote, as the db holds it.
struct Written {
    /// The tool's answer.
    out: Value,
    /// The written note's JSON, read back from the db.
    note: Value,
}

impl World {
    /// Call the write `name` with `args` as alice, then wait for the note it
    /// wrote (of `kind`) to land in the db.
    fn write(&mut self, name: &str, args: Value, kind: u64) -> Written {
        let sub = {
            let ctx = self.host.app_context();
            ctx.ndb
                .subscribe(&[Filter::new()
                    .authors([self.alice.pubkey.bytes()])
                    .kinds([kind])
                    .build()])
                .expect("subscribe")
        };
        let out = self.call(name, args).expect("write");
        let ctx = self.host.app_context();
        let mut stream = SubscriptionStream::new(ctx.ndb.clone(), sub);
        notedeck_testing::await_notes(&mut stream, 1);

        // A follow answers its note under `sent`; the others at the top.
        let id = out["id"]
            .as_str()
            .or(out["sent"]["id"].as_str())
            .expect("id");
        let id = hex::decode(id).expect("hex");
        let txn = Transaction::new(ctx.ndb).expect("txn");
        let note = ctx
            .ndb
            .get_note_by_id(&txn, &id.try_into().expect("32 bytes"))
            .expect("the written note is in the db");
        let note: Value = serde_json::from_str(&note.json().expect("json")).expect("note json");
        Written { out, note }
    }
}

/// The tags of a note's JSON whose name is `name`, each as its strings.
fn tags_named(note: &Value, name: &str) -> Vec<Vec<String>> {
    note["tags"]
        .as_array()
        .expect("tags")
        .iter()
        .map(|t| {
            t.as_array()
                .expect("tag")
                .iter()
                .map(|v| v.as_str().unwrap_or_default().to_owned())
                .collect::<Vec<_>>()
        })
        .filter(|t| t.first().map(String::as_str) == Some(name))
        .collect()
}

/// A post is a kind 1 signed by the account, its hashtags tagged, stored
/// locally at once, and the answer names it.
#[test]
fn post_signs_a_note_as_the_account_and_stores_it() {
    let mut w = world();
    let Written { out, note } = w.write(
        "nostr_post",
        json!({ "content": "  hello from the agent #notedeck  " }),
        1,
    );
    assert_eq!(out["kind"], json!(1));
    assert!(out["note"].as_str().unwrap().starts_with("note1"));
    assert_eq!(note["pubkey"], json!(w.alice.pubkey.hex()));
    assert_eq!(note["content"], json!("hello from the agent #notedeck"));
    assert_eq!(tags_named(&note, "t"), [["t", "notedeck"]]);

    let err = w
        .call("nostr_post", json!({ "content": "  " }))
        .unwrap_err();
    assert!(err.contains("needs some content"), "{err}");
}

/// A reply threads under its parent's root and tags everyone in the
/// thread; a note the db lacks is refused, not guessed at.
#[test]
fn reply_threads_under_the_root_and_needs_the_parent() {
    let mut w = world();
    let parent = nostrdb_net::NoteId::new(w.carol_reply).to_bech().unwrap();
    let Written { note, .. } = w.write(
        "nostr_reply",
        json!({ "note": parent, "content": "agreed" }),
        1,
    );
    let e = tags_named(&note, "e");
    assert!(e.contains(&vec![
        "e".into(),
        hex::encode(w.root),
        String::new(),
        "root".into()
    ]));
    assert!(e.contains(&vec![
        "e".into(),
        hex::encode(w.carol_reply),
        String::new(),
        "reply".into()
    ]));
    let p: Vec<_> = tags_named(&note, "p")
        .into_iter()
        .map(|t| t[1].clone())
        .collect();
    assert!(p.contains(&w.carol.pubkey.hex()), "{p:?}");
    assert!(p.contains(&w.bob.pubkey.hex()), "{p:?}");

    let unseen = nostrdb_net::NoteId::new([9; 32]).hex();
    let err = w
        .call("nostr_reply", json!({ "note": unseen, "content": "hi" }))
        .unwrap_err();
    assert!(err.contains("isn't in this notedeck's db"), "{err}");
    assert!(err.contains("fetching it from the user's relays"), "{err}");
}

/// A reaction is a like by default, tags the note and its author, and
/// refuses a sentence.
#[test]
fn react_likes_by_default_and_tags_the_note() {
    let mut w = world();
    let Written { out, note } = w.write("nostr_react", json!({ "note": hex::encode(w.root) }), 7);
    assert_eq!(out["kind"], json!(7));
    assert_eq!(note["content"], json!("+"));
    assert_eq!(tags_named(&note, "e")[0][1], hex::encode(w.root));
    assert_eq!(tags_named(&note, "p")[0][1], w.bob.pubkey.hex());
    assert_eq!(tags_named(&note, "k"), [["k", "1"]]);

    let err = w
        .call(
            "nostr_react",
            json!({ "note": hex::encode(w.root), "reaction": "nice one" }),
        )
        .unwrap_err();
    assert!(err.contains("not a reaction"), "{err}");
}

/// A follow republishes the contact list with the key added and the rest
/// kept; following someone already followed sends nothing.
#[test]
fn follow_adds_to_the_contact_list_and_keeps_the_rest() {
    let mut w = world();
    let Written { out, note } = w.write("nostr_follow", json!({ "who": w.carol.pubkey.npub() }), 3);
    assert_eq!(out["following"], json!(true));
    assert_eq!(out["sent"]["kind"], json!(3));
    let p: Vec<_> = tags_named(&note, "p")
        .into_iter()
        .map(|t| t[1].clone())
        .collect();
    assert_eq!(p, [w.bob.pubkey.hex(), w.carol.pubkey.hex()]);

    let again = w
        .call("nostr_follow", json!({ "who": w.bob.pubkey.hex() }))
        .unwrap();
    assert_eq!(again["following"], json!(true));
    assert_eq!(again["sent"], Value::Null);
}

/// An unfollow republishes the contact list without the key; unfollowing
/// someone not followed sends nothing.
#[test]
fn unfollow_drops_from_the_contact_list() {
    let mut w = world();
    let Written { out, note } = w.write("nostr_unfollow", json!({ "who": w.bob.pubkey.hex() }), 3);
    assert_eq!(out["following"], json!(false));
    assert!(tags_named(&note, "p").is_empty(), "{note}");

    let again = w
        .call("nostr_unfollow", json!({ "who": w.carol.pubkey.hex() }))
        .unwrap();
    assert_eq!(again["sent"], Value::Null);
}

/// With no contact list of the account's in the db, a follow refuses
/// rather than publishing a list of one that would drop every other
/// follow.
#[test]
fn follow_refuses_without_the_accounts_contact_list() {
    let dave = FullKeypair::generate();
    let carol = FullKeypair::generate();
    let mut host = TestHost::with_account(&dave);
    let tool = tools()
        .into_iter()
        .find(|t| t.name() == "nostr_follow")
        .unwrap();
    let mut ctx = host.app_context();
    let mut cx = ctx.tool_context();
    let ToolOutcome::Error(err) =
        RegisteredTool::call(&tool, &mut cx, &json!({ "who": carol.pubkey.hex() }))
    else {
        panic!("a follow without a contact list went through");
    };
    assert!(err.contains("doesn't have your contact list"), "{err}");
}

impl World {
    /// Press the profile's follow (or unfollow) button on `target` as
    /// alice, and read back the contact list it wrote and how many relays
    /// it went to.
    fn press_follow(&mut self, target: &Pubkey, follow: bool) -> Written {
        // Alice was selected before her contact list landed; selecting her
        // again reads it, as a restart would.
        let other = FullKeypair::generate();
        let mut ctx = self.host.app_context();
        notedeck_testing::fixtures::add_account(&mut ctx, &other);
        ctx.select_account(&other.pubkey);
        ctx.select_account(&self.alice.pubkey);

        let sub = ctx
            .ndb
            .subscribe(&[Filter::new()
                .authors([self.alice.pubkey.bytes()])
                .kinds([3])
                .build()])
            .expect("subscribe");
        let sent = crate::profile::send_kind_3_event(
            ctx.ndb,
            ctx.accounts,
            &mut ctx.remote,
            target,
            follow,
        )
        .expect("the button sends");
        let mut stream = SubscriptionStream::new(ctx.ndb.clone(), sub);
        notedeck_testing::await_notes(&mut stream, 1);

        let txn = Transaction::new(ctx.ndb).expect("txn");
        let note = ctx
            .ndb
            .get_note_by_id(&txn, sent.id.bytes())
            .expect("the written note is in the db");
        Written {
            out: json!({ "relays": sent.relays }),
            note: serde_json::from_str(&note.json().expect("json")).expect("note json"),
        }
    }
}

/// The profile's follow button and the `nostr_follow`/`nostr_unfollow`
/// tools are one sender: from the same contact list they write the same
/// tags and content, to as many relays.
#[test]
fn the_follow_button_and_the_follow_tools_write_the_same_contact_list() {
    let (alice, bob, carol) = (
        FullKeypair::generate(),
        FullKeypair::generate(),
        FullKeypair::generate(),
    );
    for (tool, target, follow) in [
        ("nostr_follow", carol.pubkey, true),
        ("nostr_unfollow", bob.pubkey, false),
    ] {
        let mut by_button = world_of(alice.clone(), bob.clone(), carol.clone());
        let button = by_button.press_follow(&target, follow);
        let mut by_tool = world_of(alice.clone(), bob.clone(), carol.clone());
        let tool_wrote = by_tool.write(tool, json!({ "who": target.hex() }), 3);

        assert_eq!(button.note["tags"], tool_wrote.note["tags"], "{tool}");
        assert_eq!(button.note["content"], tool_wrote.note["content"], "{tool}");
        assert_eq!(
            button.out["relays"], tool_wrote.out["sent"]["relays"],
            "{tool}"
        );
    }
}

/// A zap answers the zap button's action for the note's author, in
/// millisats; a note the db lacks has no known author, so it is fetched
/// and the zap refused until it lands.
#[test]
fn zap_raises_the_zap_buttons_action_for_the_author() {
    use notedeck::{AppAction, NoteAction, ZapAction};

    let mut w = world();
    let mut ctx = w.host.app_context();
    let root = nostrdb_net::NoteId::new(w.root);
    let outcome = act::call_ui(
        &mut ctx,
        act::ZAP,
        &json!({ "note": root.hex(), "sats": 21 }),
    );
    let Some(ToolOutcome::Intent(AppAction::Note(NoteAction::Zap(ZapAction::Send(zap))))) = outcome
    else {
        panic!("a zap didn't raise the zap action");
    };
    assert_eq!(zap.target.note_id, root);
    assert_eq!(zap.target.zap_recipient, w.bob.pubkey);
    assert_eq!(zap.specified_msats, Some(21_000));

    let unseen = nostrdb_net::NoteId::new([9; 32]).hex();
    let Some(ToolOutcome::Error(err)) =
        act::call_ui(&mut ctx, act::ZAP, &json!({ "note": unseen }))
    else {
        panic!("a zap of an unseen note went through");
    };
    assert!(err.contains("isn't in this notedeck's db"), "{err}");
    assert!(err.contains("try again"), "{err}");
    assert!(act::call_ui(&mut ctx, open::OPEN_THREAD, &json!({})).is_none());
}

// ---------------------------------------------------------------------------
// fetch on a miss
// ---------------------------------------------------------------------------

impl World {
    /// Start the read `name` with `args` as the socket or Dave does,
    /// with fetches waited on for at most `bound`, and hand back its
    /// pending job, which owns everything it needs: the call's context is
    /// gone (and its REQ flushed) before the job runs.
    fn start(&mut self, name: &str, args: Value, bound: Duration) -> notedeck::PendingCall {
        let tool = tools_within(bound)
            .into_iter()
            .find(|t| t.name() == name)
            .expect("tool");
        let mut ctx = self.host.app_context();
        let mut cx = ctx.tool_context();
        match tool.start_rendered(&mut cx, &args) {
            Started::Pending(pending) => pending,
            Started::Done(_) => panic!("a read is a slow tool, so it starts pending"),
        }
    }

    /// Ingest `frame` as if a relay had answered with it.
    fn land(&mut self, frame: &str) {
        let ctx = self.host.app_context();
        ctx.ndb
            .process_event_with(frame, IngestMetadata::new().client(true))
            .expect("ingest");
    }
}

/// A profile the db lacks is fetched: one that lands after the call was
/// prepared (as a relay's answer would) is what the job answers.
#[test]
fn get_profile_answers_a_profile_that_lands_after_the_miss() {
    let mut w = world();
    let dave = FullKeypair::generate();
    let pending = w.start(
        "nostr_get_profile",
        json!({ "who": dave.pubkey.hex() }),
        Duration::from_secs(30),
    );
    w.land(&signed(&dave, 0, r#"{"name":"dave"}"#, T0, &[]));
    let out = pending.run().result.expect("profile");
    assert_eq!(out["found"], json!(true));
    assert_eq!(out["name"], json!("dave"));
}

/// A profile nobody answers with is `found: false` once the bound passes,
/// not a hang.
#[test]
fn get_profile_answers_not_found_when_the_fetch_times_out() {
    let mut w = world();
    let stranger = FullKeypair::generate().pubkey.hex();
    let started = Instant::now();
    let pending = w.start(
        "nostr_get_profile",
        json!({ "who": stranger }),
        Duration::from_millis(200),
    );
    let out = pending.run().result.expect("profile");
    assert_eq!(out["found"], json!(false));
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(200), "{took:?}");
    assert!(took < Duration::from_secs(10), "{took:?}");
}

/// A profile the db has answers without waiting on any fetch.
#[test]
fn get_profile_answers_a_hit_without_waiting() {
    let mut w = world();
    let started = Instant::now();
    let pending = w.start(
        "nostr_get_profile",
        json!({ "who": w.bob.pubkey.hex() }),
        Duration::from_secs(30),
    );
    let out = pending.run().result.expect("profile");
    assert_eq!(out["found"], json!(true));
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// A context that can't reach the relays answers from the db alone, at
/// once.
#[test]
fn a_read_with_no_relays_answers_from_the_db_alone() {
    let mut w = world();
    let tool = tools_within(Duration::from_secs(30))
        .into_iter()
        .find(|t| t.name() == "nostr_get_profile")
        .expect("tool");
    let mut ctx = w.host.app_context();
    let mut cx = ctx.tool_context();
    cx.remote = None;
    let started = Instant::now();
    let stranger = FullKeypair::generate().pubkey.hex();
    let ToolOutcome::Data(out) = tool.call(&mut cx, &json!({ "who": stranger })) else {
        panic!("the read failed");
    };
    assert_eq!(out["found"], json!(false));
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// A thread whose note the db lacks is fetched: the note landing after
/// the miss is the thread the job answers, its own parent named missing.
#[test]
fn load_thread_answers_a_note_that_lands_after_the_miss() {
    let mut w = world();
    let dave = FullKeypair::generate();
    let reply = signed(
        &dave,
        1,
        "late to the thread",
        T0 + 40,
        &[&["e", &hex::encode(w.root), "", "root"]],
    );
    let pending = w.start(
        "nostr_load_thread",
        json!({ "note": hex::encode(id_of(&reply)) }),
        Duration::from_secs(30),
    );
    w.land(&reply);
    let out = pending.run().result.expect("thread");
    assert_eq!(out["focus"], json!(hex::encode(id_of(&reply))));
    assert_eq!(out["root"], json!(hex::encode(w.root)));
    assert!(note_ids(&out).contains(&hex::encode(w.root)));
    assert_eq!(out["missing"], json!([]));
}

/// An author timeline the db has nothing of is fetched, and the notes
/// that land answer it.
#[test]
fn read_timeline_answers_notes_that_land_after_the_miss() {
    let mut w = world();
    let dave = FullKeypair::generate();
    let pending = w.start(
        "nostr_read_timeline",
        json!({ "author": dave.pubkey.hex(), "limit": 1 }),
        Duration::from_secs(30),
    );
    let post = signed(&dave, 1, "hello from dave", T0, &[]);
    w.land(&post);
    let out = pending.run().result.expect("timeline");
    assert_eq!(note_ids(&out), vec![hex::encode(id_of(&post))]);
}
