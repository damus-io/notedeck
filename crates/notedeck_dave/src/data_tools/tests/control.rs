//! The `agentium` CLI's control cases (`send`, `spawn`, `resume`,
//! `interrupt`, `approve`/`deny`, `mode`, `link`/`unlink`, `config`), ported
//! to the tools that replace it. Each call runs through the registry against
//! the same fresh-account notedeck as the reads, and the test reads back the
//! event the call published from nostrdb, where the engine's PNS ingest left
//! it. No Dave host is running, so a test that needs a host's answer seeds
//! it.

use super::*;
use agentium_core::session_events::get_tag_value;
use std::collections::HashMap;

/// A working session, a deleted one, and where they run.
const WORKING: &str = "sess-working";
const IDLE: &str = "sess-idle";
const GONE: &str = "sess-gone";
const PROJ: &str = "/home/u/proj";

/// One published event: its content and the first value of each tag.
struct Event {
    content: String,
    tags: HashMap<String, String>,
}

impl Event {
    fn tag(&self, name: &str) -> Option<&str> {
        self.tags.get(name).map(String::as_str)
    }
}

impl Live {
    /// Wait for the `n` events a call just published to be queryable.
    fn expect(&mut self, n: usize) {
        self.pending += n;
        self.settle();
    }

    /// Every event of `kind` this account signed whose `tag` is `value`.
    fn events(&mut self, kind: u32, tag: &str, value: &str) -> Vec<Event> {
        let ndb = self.notedeck.app_context().ndb.clone();
        let txn = Transaction::new(&ndb).expect("txn");
        let author = FullKeypair::from_secret_bytes(&self.secret)
            .expect("key")
            .pubkey;
        let filter = Filter::new()
            .kinds([kind as u64])
            .authors([author.bytes()])
            .build();
        ndb.query(&txn, &[filter], 500)
            .expect("query")
            .into_iter()
            .filter_map(|r| {
                let note = r.note;
                if get_tag_value(&note, tag) != Some(value) {
                    return None;
                }
                let tags = note
                    .tags()
                    .iter()
                    .filter_map(|t| Some((t.get_str(0)?.to_owned(), t.get_str(1)?.to_owned())))
                    .collect();
                Some(Event {
                    content: note.content().to_owned(),
                    tags,
                })
            })
            .collect()
    }

    /// The kind-1988 events on session `d` with `role`.
    fn conversation(&mut self, d: &str, role: &str) -> Vec<Event> {
        self.events(AI_CONVERSATION_KIND, "d", d)
            .into_iter()
            .filter(|e| e.tag("role") == Some(role))
            .collect()
    }

    /// Seed a kind-31988 state for `d` that names extra tags and its own
    /// `created_at`, as a host republishing it would.
    fn state_at(&mut self, d: &str, status: &str, created_at: u64, extra: &[[&str; 2]]) {
        let mut builder = NoteBuilder::new()
            .kind(AI_SESSION_STATE_KIND)
            .content("")
            .created_at(created_at);
        for [name, value] in [
            ["d", d],
            ["title", "Seeded"],
            ["status", status],
            ["hostname", HOST],
            ["cwd", PROJ],
            ["backend", "claude"],
        ]
        .iter()
        .chain(extra)
        {
            builder = builder.start_tag().tag_str(name).tag_str(value);
        }
        let note = builder.sign(&self.secret).build().expect("build state");
        self.ingest(&note.json().expect("note json"));
    }

    /// Start `name` as the socket does, leaving a slow tool's work pending.
    fn start(&mut self, name: &str, args: Value) -> notedeck::Started {
        if self.pending > 0 {
            self.settle();
        }
        let tool = tools()
            .into_iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| panic!("{name} is registered"));
        let mut app = self.notedeck.app_context();
        let mut cx = app.tool_context();
        tool.start_rendered(&mut cx, &args)
    }
}

/// A notedeck with [`WORKING`], [`IDLE`] and the deleted [`GONE`].
fn sessions() -> Live {
    let mut live = live();
    live.state(WORKING, "Working away", "working", PROJ);
    live.state(IDLE, "Sitting idle", "idle", PROJ);
    live.state(GONE, "Long gone", "deleted", PROJ);
    live
}

fn uri(d: &str) -> String {
    agentium_core::wordid::session_ref(d)
}

/// A hex event id, as every write answers with.
fn assert_event_id(json: &Value) {
    let id = json["event_id"].as_str().expect("event_id");
    assert_eq!(id.len(), 64, "{id}");
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
}

// ---------------------------------------------------------------------------
// send
// ---------------------------------------------------------------------------

/// A send lands as the session's `user` message, stamped with the sending
/// session; `immediate` interrupts a working session after it.
#[test]
fn send_lands_a_user_message_and_interrupts_a_working_turn() {
    let mut live = sessions();
    let sent = live.call(
        "agentium_send",
        json!({
            "session": uri(WORKING),
            "text": "please rebase first",
            "immediate": true,
            "from_session": "agentium:some-parent-ref",
        }),
    );
    live.expect(2);

    assert_eq!(sent.json["session"], uri(WORKING));
    assert_event_id(&sent.json);
    assert_eq!(sent.json["interrupted"], true);
    assert!(sent
        .text
        .starts_with(&format!("sent to {} (event ", uri(WORKING))));
    assert!(sent.text.contains(&format!("interrupted {}", uri(WORKING))));

    let users = live.conversation(WORKING, "user");
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].content, "please rebase first");
    assert!(
        users[0]
            .tags
            .values()
            .any(|v| v == "agentium:some-parent-ref"),
        "the sender rides the message"
    );
    assert_eq!(live.conversation(WORKING, "interrupt").len(), 1);
}

/// `immediate` on an idle session doesn't interrupt (that would abort the
/// message's own turn), and a plain send answers no `interrupted`.
#[test]
fn send_never_interrupts_an_idle_session() {
    let mut live = sessions();
    let sent = live.call(
        "agentium_send",
        json!({ "session": IDLE, "text": "hi", "immediate": true }),
    );
    live.expect(1);
    assert_eq!(sent.json["interrupted"], false);
    assert!(sent
        .text
        .contains("is idle, so the message starts its turn"));
    assert!(live.conversation(IDLE, "interrupt").is_empty());

    let plain = live.call("agentium_send", json!({ "session": IDLE, "text": "again" }));
    live.expect(1);
    assert!(plain.json.get("interrupted").is_none(), "{}", plain.json);
}

/// A deleted session points at `resume`; an unknown one and a blank message
/// fail; nothing is published.
#[test]
fn send_refuses_deleted_unknown_and_empty() {
    let mut live = sessions();
    let err = live
        .try_call(
            "agentium_send",
            json!({ "session": GONE, "text": "hello?" }),
        )
        .unwrap_err();
    assert!(
        err.contains("is deleted") && err.contains("agentium resume"),
        "{err}"
    );
    assert!(live
        .try_call("agentium_send", json!({ "session": "nope", "text": "x" }))
        .is_err());
    assert!(live
        .try_call("agentium_send", json!({ "session": IDLE, "text": "  " }))
        .is_err());
    assert!(live.conversation(GONE, "user").is_empty());
    assert!(live.conversation(IDLE, "user").is_empty());
}

// ---------------------------------------------------------------------------
// interrupt / resume
// ---------------------------------------------------------------------------

#[test]
fn interrupt_lands_and_refuses_a_deleted_session() {
    let mut live = sessions();
    let done = live.call("agentium_interrupt", json!({ "session": WORKING }));
    live.expect(1);
    assert_eq!(done.json, json!({ "session": uri(WORKING) }));
    assert_eq!(done.text, format!("interrupt sent to {}\n", uri(WORKING)));
    assert_eq!(live.conversation(WORKING, "interrupt").len(), 1);

    let err = live
        .try_call("agentium_interrupt", json!({ "session": GONE }))
        .unwrap_err();
    assert!(err.contains("nothing is running to interrupt"), "{err}");

    let err = live
        .try_call(
            "agentium_interrupt",
            json!({ "session": "agentium:no-such-session" }),
        )
        .unwrap_err();
    assert!(err.contains("no session matching"), "{err}");
    assert_eq!(
        live.conversation(WORKING, "interrupt").len(),
        1,
        "an unknown session publishes nothing"
    );
}

/// A deleted session resumes: a `resume_session` command for its host,
/// naming it.
#[test]
fn resume_targets_the_sessions_host() {
    let mut live = sessions();
    let done = live.call("agentium_resume", json!({ "session": uri(GONE) }));
    live.expect(1);
    assert_eq!(done.json, json!({ "session": uri(GONE), "host": HOST }));
    assert_eq!(
        done.text,
        format!("resume command sent to {HOST} for {}\n", uri(GONE))
    );
    let commands = live.events(AI_SESSION_COMMAND_KIND, "command", "resume_session");
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].tag("target_host"), Some(HOST));
    assert_eq!(commands[0].tag("session_id"), Some(GONE));

    let err = live
        .try_call("agentium_resume", json!({ "session": "no-such-session" }))
        .unwrap_err();
    assert!(err.contains("no session"), "{err}");
}

// ---------------------------------------------------------------------------
// approve / deny / mode
// ---------------------------------------------------------------------------

/// Seed a pending `tool` request on [`WORKING`], returning its id.
fn request(live: &mut Live, tool: &str) -> uuid::Uuid {
    let secret = live.secret;
    let perm_id = uuid::Uuid::new_v4();
    let req = build_permission_request_event(
        &perm_id,
        tool,
        &json!({ "command": "rm -rf target" }),
        WORKING,
        &mut ThreadingState::new(),
        &secret,
    )
    .expect("request event");
    live.ingest(&req.note_json);
    perm_id
}

#[test]
fn approve_and_deny_answer_the_pending_request() {
    let mut live = sessions();
    let perm = request(&mut live, "Bash");

    let approved = live.call("agentium_approve", json!({ "session": WORKING }));
    live.expect(1);
    assert_eq!(approved.json["perm_id"], perm.to_string());
    assert_eq!(approved.json["tool_name"], "Bash");
    assert_eq!(approved.json["decision"], "approved");
    assert_eq!(approved.json["interrupt"], false);
    assert_event_id(&approved.json);
    assert!(
        approved.text.starts_with(&format!(
            "approved Bash request {} on ",
            &perm.to_string()[..8]
        )),
        "{}",
        approved.text
    );
    let responses = live.conversation(WORKING, "permission_response");
    assert_eq!(responses.len(), 1);

    // The answer is recorded, so a second request is the one pending now.
    let second = request(&mut live, "Edit");
    let denied = live.call(
        "agentium_deny",
        json!({
            "session": WORKING,
            "request": &second.to_string()[..8],
            "message": "not that file",
            "interrupt": true,
        }),
    );
    live.expect(1);
    assert_eq!(denied.json["decision"], "denied");
    assert_eq!(denied.json["interrupt"], true);
    assert!(
        denied.text.contains("and stopped the turn"),
        "{}",
        denied.text
    );
}

#[test]
fn respond_refuses_nothing_pending_unknown_request_and_question_approve() {
    let mut live = sessions();
    let err = live
        .try_call("agentium_approve", json!({ "session": IDLE }))
        .unwrap_err();
    assert!(err.contains("no pending permission request"), "{err}");

    request(&mut live, "AskUserQuestion");
    let err = live
        .try_call("agentium_approve", json!({ "session": WORKING }))
        .unwrap_err();
    assert!(err.contains("AskUserQuestion"), "{err}");
    let err = live
        .try_call(
            "agentium_deny",
            json!({ "session": WORKING, "request": "ffffffff" }),
        )
        .unwrap_err();
    assert!(err.contains("matches no"), "{err}");

    // Denying a question is fine.
    live.call("agentium_deny", json!({ "session": WORKING }));
    live.expect(1);
}

/// An alias is normalized before it is published; an unknown mode never is.
#[test]
fn mode_normalizes_and_refuses_unknown() {
    let mut live = sessions();
    let set = live.call(
        "agentium_set_mode",
        json!({ "session": WORKING, "mode": "acceptEdits" }),
    );
    live.expect(1);
    assert_eq!(set.json["mode"], "accept_edits");
    assert!(set
        .text
        .starts_with(&format!("set {} to accept_edits mode", uri(WORKING))));
    let modes = live.conversation(WORKING, "set_permission_mode");
    assert_eq!(modes.len(), 1);
    assert!(
        modes[0].content.contains("accept_edits"),
        "{}",
        modes[0].content
    );

    let err = live
        .try_call(
            "agentium_set_mode",
            json!({ "session": WORKING, "mode": "yolo" }),
        )
        .unwrap_err();
    assert!(err.contains("unknown permission mode"), "{err}");
}

// ---------------------------------------------------------------------------
// link / unlink
// ---------------------------------------------------------------------------

/// `link` succeeds only once the host republishes the session with the
/// issue; here the test plays the host.
#[test]
fn link_waits_for_the_host_to_republish() {
    let mut live = sessions();
    let issue = "headway:notedeck/some-card-ref";
    let notedeck::Started::Pending(pending) = live.start(
        "agentium_link",
        json!({ "session": WORKING, "issue_url": issue }),
    ) else {
        panic!("a link that changes something waits for the host");
    };
    live.expect(1);
    assert_eq!(live.conversation(WORKING, "set_issue").len(), 1);

    live.state_at(WORKING, "working", now_secs() + 5, &[["issue_url", issue]]);
    let done = pending.run();
    let json = done.result.expect("the host answered");
    assert_eq!(json["issue_url"], issue);
    assert_eq!(json["unchanged"], false);
    assert_event_id(&json);
    assert_eq!(
        done.text.as_deref(),
        Some(format!("linked {} to {issue}\n", uri(WORKING)).as_str())
    );

    // Linking it again changes nothing, so publishes nothing and waits for
    // nothing.
    let again = live.call(
        "agentium_link",
        json!({ "session": WORKING, "issue_url": issue }),
    );
    assert_eq!(again.json["unchanged"], true);
    assert!(again.json["event_id"].is_null());
    assert!(again.text.ends_with("(already)\n"), "{}", again.text);
}

#[test]
fn link_refuses_a_bare_word_id_and_unlink_of_an_unlinked_session_is_a_no_op() {
    let mut live = sessions();
    let err = live
        .try_call(
            "agentium_link",
            json!({ "session": WORKING, "issue_url": "some-card-ref" }),
        )
        .unwrap_err();
    assert!(err.contains("is not a URI"), "{err}");

    let done = live.call("agentium_unlink", json!({ "session": WORKING }));
    assert_eq!(done.json["unchanged"], true);
    assert_eq!(done.text, format!("unlinked {} (already)\n", uri(WORKING)));
}

// ---------------------------------------------------------------------------
// spawn
// ---------------------------------------------------------------------------

/// A spawn without a wait answers its `spawn_id` and a null `session`, and
/// publishes a command carrying every option.
#[test]
fn spawn_publishes_the_command_with_its_options() {
    let mut live = sessions();
    let done = live.call(
        "agentium_spawn",
        json!({
            "host": "studio",
            "cwd": "/srv/repo",
            "title": "Port the tests",
            "permission_mode": "acceptEdits",
            "model": "opus",
            "issue_url": "headway:notedeck/some-card-ref",
        }),
    );
    live.expect(1);
    assert!(done.json["session"].is_null());
    assert_eq!(done.json["host"], "studio");
    let spawn_id = done.json["spawn_id"].as_str().expect("spawn_id").to_owned();
    assert!(done
        .text
        .starts_with("spawn command sent to studio (spawn "));

    let commands = live.events(AI_SESSION_COMMAND_KIND, "spawn_id", &spawn_id);
    assert_eq!(commands.len(), 1);
    let c = &commands[0];
    assert_eq!(c.tag("command"), Some("spawn_session"));
    assert_eq!(c.tag("target_host"), Some("studio"));
    assert_eq!(c.tag("cwd"), Some("/srv/repo"));
    assert_eq!(c.tag("backend"), Some("claude"));
    assert_eq!(c.tag("custom_title"), Some("Port the tests"));
    assert_eq!(c.tag("permission_mode"), Some("accept_edits"));
    assert_eq!(c.tag("model"), Some("opus"));
    assert_eq!(c.tag("issue_url"), Some("headway:notedeck/some-card-ref"));
    assert!(
        c.tag("idempotency_key").is_some(),
        "a retry is recognizable"
    );
}

/// The target defaults to the session the caller runs inside, which is
/// recorded as the parent; a relative `cwd` is the caller's.
#[test]
fn spawn_defaults_to_the_current_session_and_records_it() {
    let mut live = sessions();
    let done = live.call(
        "agentium_spawn",
        json!({ "from_session": uri(WORKING), "successor": true }),
    );
    live.expect(1);
    let spawn_id = done.json["spawn_id"].as_str().unwrap().to_owned();
    let c = &live.events(AI_SESSION_COMMAND_KIND, "spawn_id", &spawn_id)[0];
    assert_eq!(c.tag("target_host"), Some(HOST));
    assert_eq!(c.tag("cwd"), Some(PROJ));
    assert_eq!(c.tag("parent-session"), Some(uri(WORKING).as_str()));
    assert_eq!(c.tag("relation"), Some("successor"));

    let done = live.call(
        "agentium_spawn",
        json!({ "host": "studio", "cwd": "sub", "caller_cwd": "/home/u/work" }),
    );
    live.expect(1);
    let spawn_id = done.json["spawn_id"].as_str().unwrap().to_owned();
    let c = &live.events(AI_SESSION_COMMAND_KIND, "spawn_id", &spawn_id)[0];
    assert_eq!(c.tag("cwd"), Some("/home/u/work/sub"));
}

#[test]
fn spawn_refuses_no_target_a_duplicate_and_both_relations() {
    let mut live = sessions();
    let err = live.try_call("agentium_spawn", json!({})).unwrap_err();
    assert!(err.contains("no host"), "{err}");

    // `sessions()` started "Working away" on HOST in PROJ just now.
    let err = live
        .try_call(
            "agentium_spawn",
            json!({ "host": HOST, "cwd": PROJ, "title": "Working away" }),
        )
        .unwrap_err();
    assert!(err.contains(&uri(WORKING)), "{err}");
    assert!(err.contains("--allow-duplicate"), "{err}");
    let anyway = live.call(
        "agentium_spawn",
        json!({ "host": HOST, "cwd": PROJ, "title": "Working away", "allow_duplicate": true }),
    );
    live.expect(1);
    let spawn_id = anyway.json["spawn_id"].as_str().unwrap().to_owned();
    let c = &live.events(AI_SESSION_COMMAND_KIND, "spawn_id", &spawn_id)[0];
    assert!(
        c.tag("idempotency_key").is_none(),
        "nor does the host dedupe it"
    );

    let err = live
        .try_call(
            "agentium_spawn",
            json!({ "host": HOST, "cwd": PROJ, "child": true, "successor": true }),
        )
        .unwrap_err();
    assert!(err.contains("exclusive"), "{err}");
}

/// The handoff pipeline: `spawn --wait --json | jq -r .session` reads the
/// new session's ref once the host answers. The wait runs off the caller's
/// thread (the socket's worker), so the test plays the host between the
/// publish and the wait.
#[test]
fn spawn_wait_answers_the_new_sessions_ref() {
    let mut live = sessions();
    let notedeck::Started::Pending(pending) = live.start(
        "agentium_spawn",
        json!({
            "host": HOST,
            "cwd": PROJ,
            "title": "Next card",
            "prompt": "Read the card.\nThen do it.",
            "wait": true,
            "wait_timeout": 20,
        }),
    ) else {
        panic!("spawn is a slow tool");
    };
    live.expect(1);
    let command = &live.events(AI_SESSION_COMMAND_KIND, "custom_title", "Next card")[0];
    assert_eq!(command.tag("prompt"), Some("Read the card.\nThen do it."));
    let spawn_id = command.tag("spawn_id").expect("spawn_id").to_owned();

    live.state_at(
        "sess-spawned",
        "idle",
        now_secs(),
        &[["spawn_id", &spawn_id]],
    );
    let done = pending.run();
    let json = done.result.expect("the host answered");
    assert_eq!(json["session"], uri("sess-spawned"));
    assert_eq!(json["spawn_id"], spawn_id);
    assert_eq!(
        done.text.as_deref(),
        Some(
            format!(
                "spawned {} on {HOST} (spawn {}…)\n",
                uri("sess-spawned"),
                &spawn_id[..8]
            )
            .as_str()
        )
    );
    assert_eq!(
        done.notice.as_deref(),
        Some(r#"prompt: 26 bytes, 2 lines, last line: "Then do it.""#)
    );
}

/// `ref` answers the bare ref; with no host answering, a bare wait fails, a
/// prompted one still answers (the host delivers the prompt whenever it
/// gets to it), and `ref` fails rather than print a spawn id.
#[test]
fn spawn_wait_timeouts_and_ref() {
    let mut live = sessions();
    let notedeck::Started::Pending(pending) = live.start(
        "agentium_spawn",
        json!({ "host": HOST, "cwd": "/a", "print_ref": true, "wait_timeout": 20 }),
    ) else {
        panic!("spawn is a slow tool");
    };
    live.expect(1);
    let spawn_id = live.events(AI_SESSION_COMMAND_KIND, "cwd", "/a")[0]
        .tag("spawn_id")
        .unwrap()
        .to_owned();
    live.state_at("sess-ref", "idle", now_secs(), &[["spawn_id", &spawn_id]]);
    let done = pending.run();
    assert_eq!(done.text, Some(format!("{}\n", uri("sess-ref"))));

    let err = live
        .try_call(
            "agentium_spawn",
            json!({ "host": HOST, "cwd": "/b", "wait": true, "wait_timeout": 1 }),
        )
        .unwrap_err();
    assert!(
        err.contains("no host answered on macbook within 1s"),
        "{err}"
    );

    let prompted = live.call(
        "agentium_spawn",
        json!({ "host": HOST, "cwd": "/c", "prompt": "hi", "wait_timeout": 1 }),
    );
    assert!(prompted.json["session"].is_null());

    let err = live
        .try_call(
            "agentium_spawn",
            json!({ "host": HOST, "cwd": "/d", "print_ref": true, "wait_timeout": 1 }),
        )
        .unwrap_err();
    assert!(err.contains("no ref to print"), "{err}");
}

// ---------------------------------------------------------------------------
// config
// ---------------------------------------------------------------------------

/// add → list → show → edit → rm, each write read back through `list`.
#[test]
fn config_add_edit_rm_round_trip() {
    let mut live = sessions();
    let added = live.call(
        "agentium_config_add",
        json!({ "name": " build ", "command": "cargo build", "from_session": uri(WORKING) }),
    );
    live.expect(1);
    assert_eq!(added.json["action"], "added");
    assert_eq!(added.json["config"]["name"], "build", "trimmed");
    assert_eq!(added.json["config"]["host"], HOST, "the session's host");
    assert_eq!(added.json["config"]["cwd"], PROJ);
    assert!(
        added.text.starts_with("added run config "),
        "{}",
        added.text
    );
    let id = added.json["config"]["id"].as_str().unwrap().to_owned();

    let list = live.call("agentium_config_list", json!({}));
    assert_eq!(list.json.as_array().map(Vec::len), Some(1));
    assert!(
        list.text.starts_with(&format!("{HOST}\n  {PROJ}\n")),
        "{}",
        list.text
    );
    assert!(list.text.contains("cargo build"));

    let shown = live.call("agentium_config_show", json!({ "config": "build" }));
    assert_eq!(shown.json["id"], id);
    assert!(
        shown.text.contains("command  cargo build\n"),
        "{}",
        shown.text
    );

    let edited = live.call(
        "agentium_config_edit",
        json!({ "config": &id[..8], "command": "cargo build --release" }),
    );
    live.expect(1);
    assert_eq!(edited.json["action"], "updated");
    let list = live.call("agentium_config_list", json!({ "host": "MAC" }));
    assert_eq!(list.json[0]["command"], "cargo build --release");

    let removed = live.call("agentium_config_rm", json!({ "config": id }));
    live.expect(1);
    assert_eq!(removed.json["action"], "removed");
    let list = live.call("agentium_config_list", json!({}));
    assert_eq!(list.json, json!([]));
    assert_eq!(list.text, "no run configs\n");
}

#[test]
fn config_refuses_no_place_blank_fields_and_unknown_configs() {
    let mut live = sessions();
    let err = live
        .try_call(
            "agentium_config_add",
            json!({ "name": "x", "command": "y" }),
        )
        .unwrap_err();
    assert!(err.contains("no host"), "{err}");
    assert!(live
        .try_call(
            "agentium_config_add",
            json!({ "name": " ", "command": "y", "host": HOST, "cwd": PROJ }),
        )
        .is_err());
    let err = live
        .try_call("agentium_config_edit", json!({ "config": "x" }))
        .unwrap_err();
    assert!(err.contains("needs a name and/or a command"), "{err}");
    let err = live
        .try_call("agentium_config_rm", json!({ "config": "zzz" }))
        .unwrap_err();
    assert!(err.contains("no run config matches 'zzz'"), "{err}");
}

/// The command paths, so `nd agentium <verb>` reaches each, and the
/// writes aren't marked read-only (a command bar offers them).
#[test]
fn control_tools_have_their_command_paths() {
    let paths: Vec<(String, bool)> = super::super::control::tools()
        .iter()
        .map(|t| (t.spec().command_path().join(" "), t.spec().is_read_only()))
        .collect();
    let expected = [
        ("agentium send", false),
        ("agentium spawn", false),
        ("agentium resume", false),
        ("agentium interrupt", false),
        ("agentium approve", false),
        ("agentium deny", false),
        ("agentium mode", false),
        ("agentium link", false),
        ("agentium unlink", false),
        ("agentium config list", true),
        ("agentium config show", true),
        ("agentium config add", false),
        ("agentium config edit", false),
        ("agentium config rm", false),
    ];
    let expected: Vec<(String, bool)> = expected.iter().map(|(p, r)| (p.to_string(), *r)).collect();
    assert_eq!(paths, expected);
}
