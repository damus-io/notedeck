//! The chrome's end of notedeck's socket (see [`notedeck_ipc`]): `nd` lists
//! the running apps' tools and calls them.
//!
//! The chrome owns the socket, not an app, because only the chrome can both
//! dispatch a tool to the app that owns it and carry out the intent a tool
//! answers with. Requests are served in [`update`](notedeck::App::update), so
//! a `--headless` notedeck answers them too; an intent that needs the window
//! waits for [`render`](notedeck::App::render), and is refused headless.
//!
//! Only the real notedeck serves: [`Chrome::serve_ipc`] is called by its
//! entry points, never by a constructor, so a test chrome leaves the socket of
//! the notedeck running on the same machine alone.

use super::actions::chrome_handle_app_action;
use super::Chrome;
use nostrdb_net::{FullKeypair, SecretKey};
use notedeck::{AppAction, AppContext, ToolOutcome, ToolSpec};
use notedeck_ipc::{IntentStatus, Reply, Request, Response, Signer, ToolInfo};

/// The socket and the requests waiting on the window. Empty until
/// [`Chrome::serve_ipc`].
#[derive(Default)]
pub(super) struct IpcServer {
    #[cfg(unix)]
    listener: Option<notedeck_ipc::Listener>,
    /// Intents waiting on the next render, with the client to tell once each
    /// is carried out. Empty but for the frame a request arrives on.
    #[cfg(unix)]
    applying: Vec<PendingIntent>,
}

/// An intent a socket request raised, held for the window.
#[cfg(unix)]
struct PendingIntent {
    action: AppAction,
    connection: notedeck_ipc::Connection,
}

/// What a request comes to: an answer now, or an intent for the window to
/// carry out before answering.
pub(super) enum Served {
    Reply(Response),
    Apply(AppAction),
}

impl Chrome {
    /// Listen on [`notedeck_ipc::socket_path`], taking it from any notedeck
    /// that had it. `waker` brings on the tick that serves a request. Returns
    /// the socket's [`Watch`](notedeck_ipc::Watch) when it is up, which a
    /// `--serve` notedeck reads to know when to exit; a failure is logged and
    /// notedeck runs on without it.
    #[cfg(unix)]
    pub fn serve_ipc(&mut self, waker: notedeck::Waker) -> Option<notedeck_ipc::Watch> {
        let path = notedeck_ipc::socket_path();
        match notedeck_ipc::Listener::bind(&path, move || waker.wake()) {
            Ok(listener) => {
                tracing::info!("serving tools on {}", path.display());
                let watch = listener.watch();
                self.ipc.listener = Some(listener);
                Some(watch)
            }
            Err(err) => {
                tracing::warn!("no tool socket at {}: {err}", path.display());
                None
            }
        }
    }

    /// There is no socket off Unix.
    #[cfg(not(unix))]
    pub fn serve_ipc(&mut self, waker: notedeck::Waker) -> Option<()> {
        let _ = waker;
        tracing::info!("the tool socket is unix only");
        None
    }

    /// Serve every request that has arrived. An intent is held for the next
    /// render, or refused if there is no window to render it.
    #[cfg(unix)]
    pub(super) fn poll_ipc(&mut self, ctx: &mut AppContext) {
        loop {
            let Some(incoming) = self.ipc.listener.as_ref().and_then(|l| l.try_recv()) else {
                return;
            };
            let notedeck_ipc::Incoming {
                request,
                mut connection,
            } = incoming;
            let response = match serve(self, ctx, request) {
                Served::Reply(response) => response,
                Served::Apply(action) => {
                    if let Some(egui) = ctx.egui {
                        egui.request_repaint();
                        self.ipc.applying.push(PendingIntent { action, connection });
                        continue;
                    }
                    Response::error("the tool acts on the window, and this notedeck is headless")
                }
            };
            if let Err(err) = connection.reply(&response) {
                tracing::debug!("ipc: the client left before its answer: {err}");
            }
        }
    }

    /// Carry out the intents socket requests raised, and tell each client.
    /// Allocates nothing when none are waiting.
    #[cfg(unix)]
    pub(super) fn apply_ipc_intents(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) {
        if self.ipc.applying.is_empty() {
            return;
        }
        for PendingIntent {
            action,
            mut connection,
        } in std::mem::take(&mut self.ipc.applying)
        {
            chrome_handle_app_action(self, ctx, action, ui);
            let applied = Response::ok(Reply::Intent {
                intent: IntentStatus::Applied,
            });
            if let Err(err) = connection.reply(&applied) {
                tracing::debug!("ipc: the client left before its answer: {err}");
            }
        }
    }
}

/// What `request` comes to. Runs tools; carries out no intent.
pub(super) fn serve(chrome: &mut Chrome, ctx: &mut AppContext, request: Request) -> Served {
    match request {
        Request::ListTools => Served::Reply(Response::ok(Reply::Tools {
            tools: ctx.tool_specs().iter().map(tool_info).collect(),
        })),
        Request::CallTool { name, args, signer } => {
            call_tool(chrome, ctx, &name, &args, signer.as_ref())
        }
    }
}

/// Run the tool `name`: a data tool here, a UI tool in the app that owns it.
/// What either answers with becomes the reply, or an intent for the window.
///
/// `signer` is the caller's comment key, which a data tool that accepts one
/// signs its comments with. A key that doesn't parse refuses the call rather
/// than letting it sign as the account.
fn call_tool(
    chrome: &mut Chrome,
    ctx: &mut AppContext,
    name: &str,
    args: &serde_json::Value,
    signer: Option<&Signer>,
) -> Served {
    let comment_signer = match signer.map(comment_key).transpose() {
        Ok(key) => key,
        Err(err) => return Served::Reply(Response::error(err)),
    };
    #[cfg(feature = "dave")]
    if name == notedeck_dave::SPAWN_SESSION {
        return Served::Reply(spawn_session(chrome, ctx, args));
    }
    // A person reads what nd prints, so ask a data tool for its text too.
    let notedeck::Rendered { outcome, text } =
        ctx.call_tool_rendered(name, args, comment_signer.as_ref());
    let outcome = match outcome {
        ToolOutcome::Intent(AppAction::CallTool(invocation)) => {
            match chrome.call_ui_tool(ctx, &invocation) {
                // One call is one hop, as with any tool the chrome runs.
                ToolOutcome::Intent(AppAction::CallTool(next)) => ToolOutcome::Error(format!(
                    "'{name}' asked to call '{}'; a tool call is one hop",
                    next.name
                )),
                outcome => outcome,
            }
        }
        outcome => outcome,
    };
    match outcome {
        ToolOutcome::Data(result) => Served::Reply(Response::ok(Reply::Result { result, text })),
        ToolOutcome::Error(err) => Served::Reply(Response::error(err)),
        ToolOutcome::Intent(action) => Served::Apply(action),
    }
}

/// The keypair `signer` names. The error never quotes the key.
fn comment_key(signer: &Signer) -> Result<FullKeypair, String> {
    SecretKey::parse(signer.comment_nsec.trim())
        .ok()
        .and_then(|secret| FullKeypair::from_secret_bytes(&secret.secret_bytes()))
        .ok_or_else(|| {
            "the call's comment signer is not a valid key (expected an nsec1… or a 64-char hex \
             secret key); refusing to sign as the account instead"
                .to_owned()
        })
}

/// Serve `nd spawn`'s call of Dave's
/// [`SPAWN_SESSION`](notedeck_dave::SPAWN_SESSION): start Dave if it isn't
/// running, then call it. Every other tool must already be running to be
/// called, but a spawn is how a terminal starts Dave, so this one opens it.
#[cfg(feature = "dave")]
fn spawn_session(chrome: &mut Chrome, ctx: &mut AppContext, args: &serde_json::Value) -> Response {
    let Some(slot) = chrome
        .apps
        .iter()
        .position(|app| matches!(app, crate::app::NotedeckApp::Dave(_)))
    else {
        return Response::error("this notedeck runs no Dave");
    };
    // The registry only names a running app's tools, and not until the next
    // frame, so call Dave by its slot.
    chrome.set_opened(slot);
    use notedeck::App;
    match chrome.apps[slot].call_ui_tool(ctx, notedeck_dave::SPAWN_SESSION, args) {
        ToolOutcome::Data(result) => Response::ok(Reply::Result { result, text: None }),
        ToolOutcome::Error(err) => Response::error(err),
        ToolOutcome::Intent(_) => Response::error("Dave answered a spawn with an intent"),
    }
}

/// `spec` as `list_tools` lists it.
fn tool_info(spec: &ToolSpec) -> ToolInfo {
    ToolInfo {
        name: spec.name().to_owned(),
        title: spec.display_title().to_owned(),
        description: spec.description().to_owned(),
        category: spec.category().map(str::to_owned),
        kind: match spec.kind() {
            notedeck::ToolKind::Data => notedeck_ipc::ToolKind::Data,
            notedeck::ToolKind::Ui => notedeck_ipc::ToolKind::Ui,
        },
        read_only: spec.is_read_only(),
        input_schema: spec.json_schema(),
        command: spec
            .command_path()
            .iter()
            .map(|word| (*word).to_owned())
            .collect(),
        accepts_signer: spec.takes_comment_signer(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notedeck::{AppResponse, AppTool, Notedeck, RegisteredTool, ToolContext, ToolInvocation};
    use notedeck_ipc::{Outcome, ToolKind};
    use serde_json::{json, Value};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A data tool: answers with its args.
    const ECHO: &str = "test_echo";
    /// A UI tool answering with data.
    const PEEK: &str = "test_peek";
    /// A UI tool answering with an intent to toggle the chrome's drawer.
    const TOGGLE: &str = "test_toggle_chrome";
    /// A UI tool answering with a call to [`PEEK`], which is never made.
    const CALL_AGAIN: &str = "test_call_again";
    /// A data tool that accepts a comment signer and answers with its pubkey.
    const WHO_SIGNS: &str = "test_who_signs";

    struct WhoSigns;

    impl AppTool for WhoSigns {
        type Args = Value;
        type Output = Option<String>;

        fn spec(&self) -> ToolSpec {
            ToolSpec::new(WHO_SIGNS, "Says who signs", Vec::new()).accepts_comment_signer()
        }

        fn call(&self, cx: &mut ToolContext, _args: Value) -> Result<Option<String>, String> {
            Ok(cx.comment_signer.map(|key| key.pubkey.hex()))
        }
    }

    struct Echo;

    impl AppTool for Echo {
        type Args = Value;
        type Output = Value;

        fn spec(&self) -> ToolSpec {
            ToolSpec::new(ECHO, "Answers with its args", Vec::new()).read_only()
        }

        fn call(&self, _cx: &mut ToolContext, args: Value) -> Result<Value, String> {
            Ok(args)
        }

        fn render(&self, out: &Value) -> Option<String> {
            Some(format!("echoed {out}\n"))
        }
    }

    /// An app with one data tool and UI tools answering each way a tool can,
    /// recording every UI call that reaches it.
    struct ToolApp {
        calls: Rc<RefCell<Vec<String>>>,
    }

    impl notedeck::App for ToolApp {
        fn render(&mut self, _ctx: &mut AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
            AppResponse::none()
        }

        fn tools(&self) -> Vec<RegisteredTool> {
            vec![RegisteredTool::new(Echo), RegisteredTool::new(WhoSigns)]
        }

        fn ui_tools(&self) -> Vec<ToolSpec> {
            [PEEK, TOGGLE, CALL_AGAIN]
                .into_iter()
                .map(|name| ToolSpec::new(name, "a test tool", Vec::new()).ui())
                .collect()
        }

        fn call_ui_tool(
            &mut self,
            _ctx: &mut AppContext<'_>,
            name: &str,
            _args: &Value,
        ) -> ToolOutcome {
            self.calls.borrow_mut().push(name.to_owned());
            match name {
                TOGGLE => ToolOutcome::Intent(AppAction::ToggleChrome),
                CALL_AGAIN => {
                    ToolOutcome::Intent(AppAction::CallTool(ToolInvocation::new(PEEK, Value::Null)))
                }
                _ => ToolOutcome::Data(json!({"peeked": true})),
            }
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        notedeck: Notedeck,
        chrome: Chrome,
        calls: Rc<RefCell<Vec<String>>>,
    }

    /// A chrome running only [`ToolApp`], its tools registered. It never
    /// serves the socket.
    fn fixture() -> Fixture {
        let dir = tempfile::TempDir::new().expect("tmp dir");
        let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
        let mut notedeck = Notedeck::init(&egui::Context::default(), dir.path(), &args);
        let calls = Rc::default();
        let mut chrome = Chrome::new_headless_with_apps(
            &mut notedeck,
            [crate::app::NotedeckApp::Other(
                "test".to_owned(),
                Box::new(ToolApp {
                    calls: Rc::clone(&calls),
                }),
            )],
        );
        let update = notedeck::App::take_tool_update(&mut chrome).expect("running set changed");
        notedeck.apply_tool_update(update);
        Fixture {
            _dir: dir,
            notedeck,
            chrome,
            calls,
        }
    }

    fn call(f: &mut Fixture, name: &str, args: Value) -> Served {
        let mut ctx = f.notedeck.app_context();
        let request = Request::CallTool {
            name: name.to_owned(),
            args,
            signer: None,
        };
        serve(&mut f.chrome, &mut ctx, request)
    }

    /// Call [`WHO_SIGNS`] with `signer`, answering with what it saw.
    fn call_signed(f: &mut Fixture, signer: Option<&str>) -> Outcome {
        let mut ctx = f.notedeck.app_context();
        let request = Request::CallTool {
            name: WHO_SIGNS.to_owned(),
            args: json!({}),
            signer: signer.map(|key| Signer {
                comment_nsec: key.to_owned(),
            }),
        };
        reply(serve(&mut f.chrome, &mut ctx, request))
    }

    fn reply(served: Served) -> Outcome {
        match served {
            Served::Reply(response) => response.outcome,
            Served::Apply(_) => panic!("expected a reply, got an intent"),
        }
    }

    #[tokio::test]
    async fn list_tools_names_data_ui_and_chrome_tools_with_schemas() {
        let mut f = fixture();
        let mut ctx = f.notedeck.app_context();
        let Outcome::Ok(Reply::Tools { tools }) =
            reply(serve(&mut f.chrome, &mut ctx, Request::ListTools))
        else {
            panic!("expected the tool list");
        };
        let find = |name: &str| {
            tools
                .iter()
                .find(|tool| tool.name == name)
                .unwrap_or_else(|| panic!("{name} is listed"))
        };

        let echo = find(ECHO);
        assert_eq!(echo.kind, ToolKind::Data);
        assert!(echo.read_only);
        assert_eq!(echo.input_schema["type"], json!("object"));
        assert_eq!(find(PEEK).kind, ToolKind::Ui);
        let shell = find(super::super::OPEN_COMMAND_BAR);
        assert_eq!(shell.category.as_deref(), Some(super::super::TOOL_CATEGORY));
    }

    #[tokio::test]
    async fn a_data_tool_answers_with_its_result() {
        let mut f = fixture();
        assert_eq!(
            reply(call(&mut f, ECHO, json!({"a": 1}))),
            Outcome::Ok(Reply::Result {
                result: json!({"a": 1}),
                text: Some("echoed {\"a\":1}\n".to_owned()),
            })
        );
        assert!(f.calls.borrow().is_empty(), "no UI tool ran");
    }

    /// A call's signer reaches the context of a tool that accepts one, as a
    /// keypair; a call without one leaves it `None`; and a key that doesn't
    /// parse refuses the call without echoing the key.
    #[tokio::test]
    async fn a_call_tool_signer_reaches_the_tool_context() {
        let mut f = fixture();
        let key = nostrdb_net::FullKeypair::generate();
        let Outcome::Ok(Reply::Result { result, .. }) =
            call_signed(&mut f, Some(&key.secret_key.to_secret_hex()))
        else {
            panic!("the signed call answers");
        };
        assert_eq!(result, json!(key.pubkey.hex()));

        let Outcome::Ok(Reply::Result { result, .. }) = call_signed(&mut f, None) else {
            panic!("the unsigned call answers");
        };
        assert_eq!(result, Value::Null);

        let Outcome::Error { message } = call_signed(&mut f, Some("nsec1notakey")) else {
            panic!("a bad key is refused");
        };
        assert!(message.contains("not a valid key"), "{message}");
        assert!(!message.contains("nsec1notakey"), "{message}");

        let mut ctx = f.notedeck.app_context();
        let Outcome::Ok(Reply::Tools { tools }) =
            reply(serve(&mut f.chrome, &mut ctx, Request::ListTools))
        else {
            panic!("expected the tool list");
        };
        let accepts = |name: &str| {
            tools
                .iter()
                .find(|t| t.name == name)
                .unwrap()
                .accepts_signer
        };
        assert!(accepts(WHO_SIGNS));
        assert!(!accepts(ECHO));
    }

    #[tokio::test]
    async fn a_ui_tool_runs_in_its_app_and_answers_with_its_data() {
        let mut f = fixture();
        assert_eq!(
            reply(call(&mut f, PEEK, Value::Null)),
            Outcome::Ok(Reply::Result {
                result: json!({"peeked": true}),
                text: None,
            })
        );
        assert_eq!(*f.calls.borrow(), vec![PEEK]);
    }

    #[tokio::test]
    async fn an_intent_is_held_for_the_window() {
        let mut f = fixture();
        let served = call(&mut f, TOGGLE, Value::Null);
        assert!(matches!(served, Served::Apply(AppAction::ToggleChrome)));
        assert!(
            !f.chrome.nav.navigating,
            "serving carries out nothing itself"
        );
    }

    #[tokio::test]
    async fn a_tool_call_is_one_hop() {
        let mut f = fixture();
        let Outcome::Error { message } = reply(call(&mut f, CALL_AGAIN, Value::Null)) else {
            panic!("expected an error");
        };
        assert!(message.contains("one hop"), "{message}");
        assert_eq!(*f.calls.borrow(), vec![CALL_AGAIN], "PEEK never ran");
    }

    #[tokio::test]
    async fn an_unknown_tool_is_an_error() {
        let mut f = fixture();
        assert!(matches!(
            reply(call(&mut f, "no_such_tool", Value::Null)),
            Outcome::Error { .. }
        ));
        assert!(f.calls.borrow().is_empty());
    }

    /// A call of `dave_spawn_session`, as `nd spawn` sends it, starts Dave,
    /// unopened until now, and opens a session in the directory it names.
    #[cfg(feature = "dave")]
    #[tokio::test]
    async fn spawn_session_starts_dave_and_opens_a_session() {
        let dir = tempfile::TempDir::new().expect("tmp dir");
        let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
        let mut notedeck = Notedeck::init(&egui::Context::default(), dir.path(), &args);
        let mut chrome = Chrome::new_headless(&args, &mut notedeck).expect("chrome");
        let dave = chrome
            .apps
            .iter()
            .position(|app| matches!(app, crate::app::NotedeckApp::Dave(_)))
            .expect("dave in the roster");
        assert!(!chrome.is_running(dave), "Dave starts closed");

        let mut ctx = notedeck.app_context();
        let spawn = |chrome: &mut Chrome, ctx: &mut AppContext, cwd: std::path::PathBuf| {
            let request = Request::CallTool {
                name: notedeck_dave::SPAWN_SESSION.to_owned(),
                args: serde_json::json!({ "cwd": cwd }),
                signer: None,
            };
            reply(serve(chrome, ctx, request))
        };
        let Outcome::Ok(Reply::Result { result, .. }) =
            spawn(&mut chrome, &mut ctx, dir.path().to_owned())
        else {
            panic!("the spawn answers with data");
        };
        assert!(result["session_id"].is_u64(), "{result}");
        assert!(chrome.is_running(dave), "the spawn started Dave");

        let Outcome::Error { message } = spawn(&mut chrome, &mut ctx, dir.path().join("missing"))
        else {
            panic!("a missing directory is refused");
        };
        assert!(message.contains("does not exist"), "{message}");
    }

    /// The whole trip: a client on a temp socket, the chrome polling it in
    /// `update` and carrying out the intent in `render`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_socket_client_gets_its_intent_applied() {
        let mut f = fixture();
        let sock = f._dir.path().join("notedeck.sock");
        f.chrome.ipc.listener = Some(notedeck_ipc::Listener::bind(&sock, || {}).unwrap());

        let client = std::thread::spawn(move || {
            notedeck_ipc::Client::connect(&sock)
                .unwrap()
                .request(&Request::CallTool {
                    name: TOGGLE.to_owned(),
                    args: Value::Null,
                    signer: None,
                })
        });

        let egui_ctx = egui::Context::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while f.chrome.ipc.applying.is_empty() {
            assert!(std::time::Instant::now() < deadline, "no request arrived");
            let mut ctx = f.notedeck.app_context();
            ctx.egui = Some(&egui_ctx);
            f.chrome.poll_ipc(&mut ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let mut ctx = f.notedeck.app_context();
        egui_ctx
            .run_ui(egui::RawInput::default(), |ui| {
                f.chrome.apply_ipc_intents(&mut ctx, ui);
            })
            .drop_without_applying_deltas();
        assert!(f.chrome.nav.navigating, "the drawer is opening");

        let response = client.join().unwrap().unwrap();
        assert_eq!(
            response.outcome,
            Outcome::Ok(Reply::Intent {
                intent: IntentStatus::Applied
            })
        );
    }
}
