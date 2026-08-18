//! A tiny end-to-end tour of the agentium-core public engine API.
//!
//! Two engines share one device key (a "desktop" that hosts a session and a
//! "phone" that observes and drives it) and talk over an in-process nostr relay.
//! It walks the whole public surface: connect → list_sessions → session_messages
//! → respond_permission → send_message → spawn_session, waiting on the
//! event-driven `watch_sessions` / `watch_session` streams (no polling).
//!
//! Run it with:
//!
//! ```sh
//! cargo run -p agentium-core --example remote_session
//! ```
//!
//! The `desktop_*` helpers here stand in for what the real host (notedeck_dave)
//! publishes — session state, a permission request. That host side is NOT part
//! of this API; the API is the phone's observer/controller surface. The desktop
//! seeds those envelopes with its own [`Session`], the same private-note pipe a
//! standalone engine owns internally.

use agentium_core::messages::Message;
use agentium_core::session_events::{
    build_permission_request_event, build_session_state_event, now_secs, wrap_pns, ThreadingState,
};
use agentium_core::{Engine, SessionWatch};
use nostrdb::{Config, Ndb};
use nostrdb_net::relay::server;
use nostrdb_net::relay::sync::Session;
use tempfile::TempDir;

/// The one device key both engines share (see the crate's identity docs).
const DEVICE_KEY: [u8; 32] = [42u8; 32];
const SESSION_ID: &str = "demo-session";

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    // --- an in-process relay both engines sync through -----------------------
    let _relay_dir = TempDir::new().unwrap();
    let relay_ndb = Ndb::new(_relay_dir.path().to_str().unwrap(), &Config::new()).unwrap();
    let relay = server::spawn(relay_ndb, "127.0.0.1:0".parse().unwrap()).unwrap();
    let url = relay.url();
    println!("relay listening at {url}\n");

    // --- two engines on the same identity ------------------------------------
    // Each standalone engine owns its `Session` — the private-note sync pipe —
    // and `connect` points it at the relay; its write methods publish through it.
    let desk_dir = TempDir::new().unwrap();
    let mut desktop = Engine::open(desk_dir.path().to_str().unwrap(), DEVICE_KEY).unwrap();
    desktop.connect(&url).unwrap();

    let phone_dir = TempDir::new().unwrap();
    let mut phone = Engine::open(phone_dir.path().to_str().unwrap(), DEVICE_KEY).unwrap();
    phone.connect(&url).unwrap();
    println!("desktop + phone connected as {}\n", phone.account_pubkey());

    // --- desktop (host) opens a session and asks for permission --------------
    // The host stand-in seeds its backend-produced envelopes with its own
    // `Session` — the same pipe a standalone engine owns internally — over a
    // throwaway db it only publishes from (its inbound leg is never subscribed).
    let seeder_dir = TempDir::new().unwrap();
    let seeder_ndb = Ndb::new(seeder_dir.path().to_str().unwrap(), &Config::new()).unwrap();
    let seeder = Session::new(seeder_ndb);
    desktop_open_session(&seeder, &url);
    let perm_id = desktop_request_permission(&seeder, &url);
    println!("desktop published a session + a `Bash` permission request\n");

    // === from here on, everything is the PHONE using the public API ==========

    // 1. await the session showing up in the list
    let mut sessions = phone.watch_sessions().unwrap();
    until(&mut sessions, || !phone.list_sessions().is_empty()).await;
    println!("phone.list_sessions():");
    for s in phone.list_sessions() {
        println!(
            "  • {}  \"{}\"  [{}] on {}",
            s.claude_session_id, s.title, s.status, s.hostname
        );
    }
    println!();

    // 2. await the permission request landing in the conversation
    let mut convo = phone.watch_session(SESSION_ID).unwrap();
    until(&mut convo, || {
        has_pending_permission(&phone.session_messages(SESSION_ID))
    })
    .await;
    println!("phone.session_messages(\"{SESSION_ID}\"):");
    print_messages(&phone.session_messages(SESSION_ID));
    println!();

    // 3. approve it, then send a follow-up
    phone
        .respond_permission(
            SESSION_ID,
            &perm_id.to_string(),
            true,
            Some("go for it".into()),
            false,
        )
        .unwrap();
    println!("phone.respond_permission(allow) — approved {perm_id}");
    phone
        .send_message(SESSION_ID, "also add a test while you're in there")
        .unwrap();
    println!("phone.send_message(\"also add a test…\")\n");

    // 4. the desktop awaits the phone's follow-up via watch_session
    let mut desk_convo = desktop.watch_session(SESSION_ID).unwrap();
    until(&mut desk_convo, || {
        desktop
            .session_messages(SESSION_ID)
            .iter()
            .any(|m| matches!(m, Message::User(u) if u.text.contains("add a test")))
    })
    .await;
    println!("desktop.watch_session woke — it now sees the phone's message\n");

    // 5. spawn a brand-new session on another host
    let spawn_id = phone
        .spawn_session("build-server", "/home/dev/project", "claude", None, None)
        .unwrap();
    println!("phone.spawn_session(\"build-server\") -> spawn_id {spawn_id}");

    relay.shutdown();
}

/// Await until `cond` holds, waking only when the watched subscription changes.
/// This is the intended usage of a [`SessionWatch`]: wait, then re-read the
/// snapshot. Returns when `cond` is satisfied (or the subscription ends).
async fn until(watch: &mut SessionWatch, cond: impl Fn() -> bool) {
    while !cond() {
        if !watch.changed().await {
            break;
        }
    }
}

fn has_pending_permission(messages: &[Message]) -> bool {
    messages
        .iter()
        .any(|m| matches!(m, Message::PermissionRequest(p) if p.response.is_none()))
}

/// Print one line per message with a short role label.
fn print_messages(messages: &[Message]) {
    for m in messages {
        match m {
            Message::User(u) => println!("  user:      {}", u.text),
            Message::Assistant(a) => println!("  assistant: {}", a.text()),
            Message::PermissionRequest(p) => {
                let state = if p.response.is_some() {
                    "resolved"
                } else {
                    "PENDING"
                };
                println!("  perm[{state}]: {} {}", p.tool_name, p.tool_input);
            }
            Message::ToolResponse(_) => println!("  tool-response"),
            other => println!("  {other:?}"),
        }
    }
}

// --- desktop/host stand-ins (not part of the public API) --------------------

/// Publish a kind-31988 session state so the phone can list it.
fn desktop_open_session(seeder: &Session, relay: &str) {
    let state = build_session_state_event(
        SESSION_ID,
        "Refactor the engine",
        None,
        "/home/dev/project",
        "working",
        None,
        "build-server",
        "/home/dev",
        "claude",
        "default",
        None,
        None,
        now_secs(),
        &DEVICE_KEY,
    )
    .unwrap();
    desktop_publish(seeder, &state.note_json, relay);
}

/// Publish a kind-1988 permission request; returns its id for the phone to answer.
fn desktop_request_permission(seeder: &Session, relay: &str) -> uuid::Uuid {
    let perm_id = uuid::Uuid::new_v4();
    let mut threading = ThreadingState::new();
    let req = build_permission_request_event(
        &perm_id,
        "Bash",
        &serde_json::json!({ "command": "cargo build --release" }),
        SESSION_ID,
        &mut threading,
        &DEVICE_KEY,
    )
    .unwrap();
    desktop_publish(seeder, &req.note_json, relay);
    perm_id
}

/// PNS-wrap a freshly-built inner event and publish it through the seeder
/// [`Session`] — the same envelope the engine's own write methods produce.
fn desktop_publish(seeder: &Session, inner_json: &str, relay: &str) {
    let pns = enostr::pns::derive_pns_keys(&DEVICE_KEY);
    let wrapped = wrap_pns(inner_json, &pns).unwrap();
    seeder.publish(wrapped, vec![relay.to_string()]);
}
