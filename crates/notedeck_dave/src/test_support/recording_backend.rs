//! [`RecordingBackend`]: an [`AiBackend`] that does nothing but write down
//! what reached it, so a test asserts on Dave's calls instead of writing
//! its own no-op backend.
//!
//! Each call lands in the backend's [`BackendLog`], which the test keeps
//! an `Arc` of after boxing the backend into Dave: the turns it was handed
//! ([`RecordedTurn`]), and how many compacts, interrupts and cleanups, and
//! which permission modes. What a turn streams back is its [`Reply`].

use crate::backend::{AiBackend, ResponseStream};
use crate::messages::{DaveApiResponse, Message};
use crate::tools::Tool;
use claude_agent_sdk_rs::PermissionMode;
use notedeck::Waker;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};

/// What a [`RecordingBackend`] streams back for a turn.
///
/// Which one matters: `dispatch_turn` installs a receiver only when the
/// backend hands one back, so `Silent` leaves the session with no stream
/// while the other two give it one.
#[derive(Clone, Debug, Default)]
pub enum Reply {
    /// No stream (`rx` is `None`): the turn's responses, if any, come from
    /// elsewhere — a script, or nowhere at all.
    #[default]
    Silent,
    /// A fresh channel whose sender the log keeps, so the test streams the
    /// turn itself ([`BackendLog::take_stream`]) and ends it by dropping
    /// the sender.
    Handed,
    /// A fresh channel carrying this one token, then closed.
    Says(String),
}

/// What one turn handed the backend.
#[derive(Clone, Debug)]
pub struct RecordedTurn {
    /// The in-process routing key (`dave-session-{n}`).
    pub session_id: String,
    /// The text of the last user message in the turn's history.
    pub last_user: Option<String>,
    /// The names of the tools the turn offered, sorted.
    pub tools: Vec<String>,
    /// The model the turn asked for.
    pub model: Option<String>,
    /// The environment the turn would export into a subprocess.
    pub env: BTreeMap<String, String>,
}

/// Everything a [`RecordingBackend`] was asked, shared with the test.
#[derive(Default)]
pub struct BackendLog {
    turns: Mutex<Vec<RecordedTurn>>,
    /// The sender of the newest [`Reply::Handed`] turn, until taken.
    stream: Mutex<Option<mpsc::Sender<DaveApiResponse>>>,
    compacts: AtomicUsize,
    interrupts: AtomicUsize,
    mode_changes: Mutex<Vec<PermissionMode>>,
    cleanups: AtomicUsize,
}

impl BackendLog {
    /// The turns handed to the backend, oldest first.
    pub fn turns(&self) -> Vec<RecordedTurn> {
        self.turns.lock().unwrap().clone()
    }

    /// The sender of the newest [`Reply::Handed`] turn, which feeds the
    /// receiver that turn installed. `None` once taken, or when no turn
    /// minted one.
    pub fn take_stream(&self) -> Option<mpsc::Sender<DaveApiResponse>> {
        self.stream.lock().unwrap().take()
    }

    /// How many times a session was asked to compact directly.
    pub fn compacts(&self) -> usize {
        self.compacts.load(Ordering::SeqCst)
    }

    /// How many times a session was interrupted.
    pub fn interrupts(&self) -> usize {
        self.interrupts.load(Ordering::SeqCst)
    }

    /// The permission modes sessions were switched to, oldest first.
    pub fn mode_changes(&self) -> Vec<PermissionMode> {
        self.mode_changes.lock().unwrap().clone()
    }

    /// How many sessions were cleaned up.
    pub fn cleanups(&self) -> usize {
        self.cleanups.load(Ordering::SeqCst)
    }
}

/// An [`AiBackend`] that records every call into its [`BackendLog`] and
/// answers each turn with its [`Reply`].
///
/// ```ignore
/// let backend = RecordingBackend::new().replying(Reply::Handed);
/// let log = backend.log();
/// dave.backends.insert(BackendType::OpenAI, Box::new(backend));
/// // ... send a message ...
/// let stream = log.take_stream().unwrap();
/// assert_eq!(log.turns()[0].last_user.as_deref(), Some("go"));
/// ```
#[derive(Default)]
pub struct RecordingBackend {
    log: Arc<BackendLog>,
    reply: Reply,
    persistent: bool,
}

impl RecordingBackend {
    /// A backend that answers every turn [`Reply::Silent`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer every turn with `reply`.
    pub fn replying(mut self, reply: Reply) -> Self {
        self.reply = reply;
        self
    }

    /// Behave like a persistent-stream backend (Claude): a session owns one
    /// channel for its whole life, so only the first turn the backend is
    /// handed gets a reply stream and every later turn gets `None`, which
    /// tells the caller to keep the receiver it holds.
    pub fn persistent(mut self) -> Self {
        self.persistent = true;
        self
    }

    /// The log, to keep after the backend is boxed into Dave.
    pub fn log(&self) -> Arc<BackendLog> {
        Arc::clone(&self.log)
    }

    /// The receiver for a turn, minting its channel per [`Self::reply`].
    fn reply_rx(&self) -> Option<mpsc::Receiver<DaveApiResponse>> {
        match &self.reply {
            Reply::Silent => None,
            Reply::Handed => {
                let (tx, rx) = mpsc::channel();
                *self.log.stream.lock().unwrap() = Some(tx);
                Some(rx)
            }
            Reply::Says(token) => {
                let (tx, rx) = mpsc::channel();
                let _ = tx.send(DaveApiResponse::Token(token.clone()));
                Some(rx)
            }
        }
    }
}

impl AiBackend for RecordingBackend {
    fn stream_request(
        &self,
        messages: Vec<Message>,
        tools: Arc<HashMap<String, Tool>>,
        model: Option<String>,
        _user_id: String,
        session_id: String,
        session_env: BTreeMap<String, String>,
        _cwd: Option<PathBuf>,
        _resume_session_id: Option<String>,
        _permission_mode: PermissionMode,
        _waker: Waker,
    ) -> ResponseStream {
        let last_user = messages.iter().rev().find_map(|m| match m {
            Message::User(u) => Some(u.text.clone()),
            _ => None,
        });
        let mut tool_names: Vec<String> = tools.keys().cloned().collect();
        tool_names.sort();
        let first = {
            let mut turns = self.log.turns.lock().unwrap();
            turns.push(RecordedTurn {
                session_id,
                last_user,
                tools: tool_names,
                model,
                env: session_env,
            });
            turns.len() == 1
        };
        let rx = if self.persistent && !first {
            None
        } else {
            self.reply_rx()
        };
        ResponseStream { rx, task: None }
    }

    fn persistent_stream(&self) -> bool {
        self.persistent
    }

    fn cleanup_session(&self, _session_id: String) {
        self.log.cleanups.fetch_add(1, Ordering::SeqCst);
    }

    fn interrupt_session(&self, _session_id: String, _waker: Waker) {
        self.log.interrupts.fetch_add(1, Ordering::SeqCst);
    }

    fn set_permission_mode(&self, _session_id: String, mode: PermissionMode, _waker: Waker) {
        self.log.mode_changes.lock().unwrap().push(mode);
    }

    fn compact_session(
        &self,
        _session_id: String,
        _waker: Waker,
    ) -> Option<mpsc::Receiver<DaveApiResponse>> {
        self.log.compacts.fetch_add(1, Ordering::SeqCst);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(backend: &RecordingBackend, text: &str) -> ResponseStream {
        let tools: HashMap<String, Tool> = crate::tools::dave_tools()
            .into_iter()
            .map(|t| (t.name().to_owned(), t))
            .collect();
        backend.stream_request(
            vec![
                Message::User("earlier".into()),
                Message::User(text.into()),
                Message::Error("not a user row".into()),
            ],
            Arc::new(tools),
            Some("m1".into()),
            "user".into(),
            "dave-session-1".into(),
            BTreeMap::from([("K".to_owned(), "V".to_owned())]),
            None,
            None,
            PermissionMode::Default,
            Waker::noop(),
        )
    }

    /// A turn, an interrupt, a mode change, a compact and a cleanup are
    /// each logged, and a silent turn hands back no stream.
    #[test]
    fn each_call_is_logged() {
        let backend = RecordingBackend::new();
        let log = backend.log();

        assert!(turn(&backend, "go").rx.is_none(), "silent: no stream");
        backend.interrupt_session("s".into(), Waker::noop());
        backend.set_permission_mode("s".into(), PermissionMode::Plan, Waker::noop());
        assert!(backend.compact_session("s".into(), Waker::noop()).is_none());
        backend.cleanup_session("s".into());

        let [t] = log.turns().try_into().expect("one turn");
        assert_eq!(t.session_id, "dave-session-1");
        assert_eq!(t.last_user.as_deref(), Some("go"));
        let mut names: Vec<String> = crate::tools::dave_tools()
            .iter()
            .map(|t| t.name().to_owned())
            .collect();
        names.sort();
        assert!(names.len() > 1, "a sort to check");
        assert_eq!(t.tools, names);
        assert_eq!(t.model.as_deref(), Some("m1"));
        assert_eq!(t.env["K"], "V");
        assert_eq!(log.interrupts(), 1);
        assert_eq!(log.mode_changes(), [PermissionMode::Plan]);
        assert_eq!(log.compacts(), 1);
        assert_eq!(log.cleanups(), 1);
        assert!(log.take_stream().is_none(), "nothing handed");
    }

    /// `Handed` gives the test the turn's sender; `Says` streams its token
    /// and closes; `persistent` streams only the first turn.
    #[test]
    fn replies_stream_as_asked() {
        let handed = RecordingBackend::new().replying(Reply::Handed);
        let rx = turn(&handed, "go").rx.expect("a stream");
        let tx = handed.log().take_stream().expect("the sender");
        tx.send(DaveApiResponse::Token("hi".into())).unwrap();
        assert!(matches!(rx.try_recv(), Ok(DaveApiResponse::Token(t)) if t == "hi"));
        assert!(handed.log().take_stream().is_none(), "taken once");

        let says = RecordingBackend::new().replying(Reply::Says("ok".into()));
        let rx = turn(&says, "go").rx.expect("a stream");
        assert!(matches!(rx.try_recv(), Ok(DaveApiResponse::Token(t)) if t == "ok"));
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));

        let persistent = RecordingBackend::new().replying(Reply::Handed).persistent();
        assert!(persistent.persistent_stream());
        assert!(turn(&persistent, "one").rx.is_some(), "the actor's channel");
        assert!(turn(&persistent, "two").rx.is_none(), "keep the one held");
        assert_eq!(persistent.log().turns().len(), 2);
    }
}
