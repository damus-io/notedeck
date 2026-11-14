//! The `agentium` CLI's streams (`log --follow`, `watch`, `watch --jsonl`,
//! `watch --once`), ported to the tools that replace them: each is
//! subscribed to through the registry, as the socket does for `nd`, and its
//! job runs on a runtime of the test's own while the test seeds notes and
//! reads what the job emits.

use super::*;
use notedeck::{Emitter, EventSink, RegisteredTool};
use std::sync::mpsc;
use std::time::Duration;

/// How long a test waits for a stream to emit what a seeded note should
/// make it emit: a backstop, since a local ingest lands in microseconds.
const WAIT: Duration = Duration::from_secs(10);

/// What a stream sent its subscriber.
#[derive(Debug)]
enum Got {
    Event(Emitted),
    Failed(String),
}

/// One event a stream emitted: its value, and its text color-stripped.
#[derive(Debug)]
struct Emitted {
    event: Value,
    text: String,
}

/// A subscriber that hands each event to the test, its text color-stripped.
struct Channel(mpsc::Sender<Got>);

impl EventSink for Channel {
    fn send(&mut self, event: Value, text: Option<String>) -> bool {
        let text = plain(&text.expect("every agentium stream renders text"));
        self.0.send(Got::Event(Emitted { event, text })).is_ok()
    }

    fn fail(&mut self, message: String) {
        let _ = self.0.send(Got::Failed(message));
    }
}

/// A running stream: what it emits, and how to hang up on it.
struct Following {
    runtime: tokio::runtime::Runtime,
    got: mpsc::Receiver<Got>,
    hang_up: tokio::sync::watch::Sender<bool>,
    job: tokio::task::JoinHandle<()>,
}

impl Following {
    /// The next event, failing the test on an error or none within [`WAIT`].
    fn next(&self) -> Got {
        self.got.recv_timeout(WAIT).expect("an event")
    }

    /// The next event's value and text.
    fn event(&self) -> Emitted {
        match self.next() {
            Got::Event(emitted) => emitted,
            Got::Failed(err) => panic!("the stream failed: {err}"),
        }
    }

    /// Hang up, as a client closing its socket does, and see the job end.
    fn hang_up(self) {
        self.hang_up.send(true).unwrap();
        self.runtime
            .block_on(async { tokio::time::timeout(WAIT, self.job).await })
            .expect("the stream ends once its subscriber hangs up")
            .expect("the job didn't panic");
    }
}

/// The registered tool `name`.
fn registered(name: &str) -> RegisteredTool {
    tools()
        .into_iter()
        .find(|tool| tool.name() == name)
        .unwrap_or_else(|| panic!("{name} is registered"))
}

impl Live {
    /// Subscribe to the stream `name` with `args`, as the socket does, or
    /// its error when it can't start.
    fn try_subscribe(&mut self, name: &str, args: Value) -> Result<Following, String> {
        if self.pending > 0 {
            self.settle();
        }
        let tool = registered(name);
        let job = {
            let mut app = self.notedeck.app_context();
            let mut cx = app.tool_context();
            tool.subscribe(&mut cx, &args)?
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime");
        let (sent, got) = mpsc::channel();
        let (hang_up, gone) = tokio::sync::watch::channel(false);
        let job = runtime.spawn(job(Emitter::new(Channel(sent), gone)));
        Ok(Following {
            runtime,
            got,
            hang_up,
            job,
        })
    }

    /// [`try_subscribe`](Self::try_subscribe), failing the test on an error.
    fn subscribe(&mut self, name: &str, args: Value) -> Following {
        self.try_subscribe(name, args.clone())
            .unwrap_or_else(|err| panic!("{name} {args}: {err}"))
    }
}

/// `log --follow`: the tail first, `--last` bounding it, then each message
/// as it lands, set off by a blank line, then a status change as a line of
/// its own; hanging up ends it.
#[test]
fn follow_streams_the_tail_then_each_new_message_and_status() {
    let mut live = live();
    seed_log(&mut live);
    let follow = live.subscribe(
        "agentium_follow_log",
        json!({ "session": "sess-log", "last": 2, "follow": true }),
    );
    let Emitted { event, text } = follow.event();
    assert_eq!(event["role"], "assistant", "--last 2 skips the first turn");
    assert_eq!(text, "assistant\n  the answer\n");
    let Emitted { event, text } = follow.event();
    assert_eq!(event["role"], "user");
    assert_eq!(text, "\nuser\n  again please\n", "set off by a blank line");

    live.message("sess-log", "assistant", "one more", 4, 4_000);
    let Emitted { event, text } = follow.event();
    assert_eq!(event["text"], "one more");
    assert_eq!(text, "\nassistant\n  one more\n");

    live.revise(
        "sess-log",
        "Ordered session",
        "needs_input",
        CWD,
        now_secs() + 1,
    );
    let Emitted { event, text } = follow.event();
    assert_eq!(event, json!({ "event": "status", "status": "needs_input" }));
    assert!(text.contains("── ◆ Needs Input ──"), "{text:?}");

    follow.hang_up();
}

/// A follow of a session that doesn't exist fails before anything streams.
#[test]
fn follow_refuses_an_unknown_session_up_front() {
    let mut live = live();
    seed_log(&mut live);
    let Err(err) = live.try_subscribe(
        "agentium_follow_log",
        json!({ "session": "no-such-session", "follow": true }),
    ) else {
        panic!("an unknown session can't be followed");
    };
    assert!(err.contains("no-such-session"), "{err}");
}

/// `watch`: the whole dashboard as one frame, the session waiting on the
/// user first, then a new frame when a session changes.
#[test]
fn watch_redraws_the_dashboard_on_each_change() {
    let mut live = live();
    live.state("sess-a", "Quiet one", "idle", CWD);
    live.state("sess-b", "Busy one", "working", CWD);
    let watch = live.subscribe("agentium_watch", json!({}));
    let Emitted { event, text } = watch.event();
    assert!(text.starts_with("agentium watch — updated "), "{text}");
    assert!(text.contains("2 sessions · 1 working · 1 idle"), "{text}");
    assert_eq!(event["sessions"][0]["title"], "Busy one");
    assert!(event["sessions"][0]["agentium_uri"].is_string());
    assert!(event["sessions"][0]["last_activity"].is_u64());

    live.revise("sess-a", "Quiet one", "needs_input", CWD, now_secs() + 1);
    let Emitted { event, text } = watch.event();
    assert!(text.contains("1 needs input · 1 working"), "{text}");
    assert_eq!(event["sessions"][0]["title"], "Quiet one", "waiting first");

    watch.hang_up();
}

/// `watch --jsonl`: the statuses it starts with print nothing, and each
/// change after is one line, `from` to `to`.
#[test]
fn watch_transitions_streams_each_status_change() {
    let mut live = live();
    live.state("sess-a", "Quiet one", "idle", CWD);
    let watch = live.subscribe("agentium_watch_transitions", json!({ "jsonl": true }));

    live.revise("sess-a", "Quiet one", "working", CWD, now_secs() + 1);
    let Emitted { event, text } = watch.event();
    assert_eq!(event["from"], "idle");
    assert_eq!(event["to"], "working");
    assert_eq!(event["session"]["title"], "Quiet one");
    let line: Value = serde_json::from_str(text.trim_end()).expect("a JSON line");
    assert_eq!(line, event);

    live.state("sess-new", "New one", "pending", CWD);
    let Emitted { event, .. } = watch.event();
    assert_eq!(event["from"], Value::Null, "first seen mid-stream");
    assert_eq!(event["to"], "pending");

    watch.hang_up();
}

/// `watch --once`: one frame, as a plain read.
#[test]
fn watch_once_answers_one_frame() {
    let mut live = live();
    live.state("sess-a", "Quiet one", "idle", CWD);
    live.state("sess-b", "Waiting one", "needs_input", CWD);
    let answer = live.call("agentium_watch_once", json!({ "once": true }));
    let lines: Vec<&str> = answer.text.lines().collect();
    assert_eq!(lines[0], "2 sessions · 1 needs input · 1 idle");
    assert!(lines[2].starts_with("» "), "{}", lines[2]);
    assert_eq!(answer.json["sessions"][0]["title"], "Waiting one");
    assert_eq!(answer.json["sessions"][1]["status"], "idle");
}

/// The streams are streams: a call of one is refused, they share their
/// command paths with the reads they extend, and each picks itself by a
/// required switch.
#[test]
fn the_streams_are_subscribed_to_on_shared_command_lines() {
    for (name, path, switch) in [
        ("agentium_follow_log", ["agentium", "log"], Some("follow")),
        ("agentium_watch", ["agentium", "watch"], None),
        (
            "agentium_watch_transitions",
            ["agentium", "watch"],
            Some("jsonl"),
        ),
    ] {
        let tool = registered(name);
        let spec = tool.spec();
        assert!(spec.stream_mode().is_some(), "{name}");
        assert!(spec.is_read_only(), "{name}");
        assert_eq!(spec.command_path(), path, "{name}");
        if let Some(switch) = switch {
            let arg = spec
                .arguments()
                .iter()
                .find(|a| a.name() == switch)
                .unwrap();
            assert!(arg.is_required(), "{name} --{switch}");
        }
    }
    let mut live = live();
    let Err(err) = live.try_call("agentium_watch", json!({})) else {
        panic!("a stream can't be called");
    };
    assert!(err.contains("nd agentium watch"), "{err}");
}
