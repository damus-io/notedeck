//! A [`Dave`] for tests, with the host pieces its per-frame work borrows.
//!
//! [`TestDave`] owns a Dave beside the store, key and waker a
//! [`HostCtx`] lends, so a test drives Dave the way the app's update loop
//! does without an `AppContext`. Dave listens on no socket (the chrome owns
//! notedeck's), so running the tests never touches the one of the notedeck
//! running on the same machine.
//!
//! Another crate's tests reach it through the `test-support` feature: chrome's
//! end-to-end key tests build one with [`TestDave::hosted`], seed it
//! ([`TestDave::add_sessions`], [`TestDave::set_issue`],
//! [`TestDave::add_subagent`], or anything else through
//! [`TestDave::session_mut`]) and hand its `dave` to the chrome. Normal mode isn't seeded: the chrome autofocuses an
//! app's input the first time it shows it, so a hosted test presses Esc
//! through the chrome, as a user does.

use crate::backend::{BackendType, RemoteOnlyBackend};
use crate::config::{AiMode, AiProvider, DaveSettings};
use crate::conversation::subscribe_conversation_events;
use crate::conversation_feed::ConversationFeed;
use crate::messages::{Message, SubagentInfo, SubagentStatus};
use crate::pns_runtime::PnsLocalState;
use crate::session::{ChatSession, SessionId, SessionMessage};
use crate::stream_events::HostCtx;
use crate::ui::{self, DaveAction, UiActionResult};
use crate::{Dave, DaveOverlay};
use nostrdb::Ndb;
use notedeck::timed_serializer::TimedSerializer;
use notedeck::{AppContext, DataPath, DataPathType, Waker};
use std::any::Any;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use tempfile::TempDir;

pub use ndb_testing::test_config;

/// The account every test signs as.
pub fn test_secret_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    key[0] = 1;
    key
}

/// A bare test [`Dave`] at `data_path`, for a test that reads no store.
///
/// The store it was built over is dropped with this call: Dave keeps no
/// handle to it under the Claude backend [`seed_agentic_settings`] picks.
/// A test that does read the store wants [`TestDave`].
pub fn test_dave(data_path: &DataPath) -> Dave {
    TestDave::at(data_path).dave
}

/// A test [`Dave`] and the host pieces a [`HostCtx`] borrows: the store,
/// the account's key and a waker nothing listens to.
///
/// Dave starts the way an agentic Dave settles once its account is known
/// and its first session picked: `pns_local_state` names [`Self::account`],
/// no overlay is open, and the session manager is empty.
pub struct TestDave {
    pub dave: Dave,
    /// The store the host hands Dave each frame; it unwraps the account's
    /// PNS envelopes.
    pub ndb: Ndb,
    pub sk: [u8; 32],
    pub account: nostrdb_net::Pubkey,
    pub waker: Waker,
    /// The settings dir, when the fixture made it, kept alive for as long
    /// as Dave saves into it.
    _data_dir: Option<TempDir>,
    /// The store's dir, when the fixture made it, kept alive with `ndb`.
    _ndb_dir: Option<TempDir>,
}

/// [`TestDave::new`]: its own settings dir and store.
impl Default for TestDave {
    fn default() -> Self {
        Self::new()
    }
}

/// A [`TestDave`]'s Dave beside the host its per-frame work takes, lent
/// together because the host borrows the fixture's store.
pub struct TestParts<'a> {
    pub dave: &'a mut Dave,
    pub host: HostCtx<'a>,
}

impl TestDave {
    /// A Dave with its own settings dir and its own store.
    pub fn new() -> Self {
        let data_dir = TempDir::new().unwrap();
        let mut test = Self::at(&DataPath::new(data_dir.path()));
        test._data_dir = Some(data_dir);
        test
    }

    /// A Dave whose settings live at `data_path`, so a test can build a
    /// second one there to see what the first persisted. It has its own store.
    pub fn at(data_path: &DataPath) -> Self {
        let ndb_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(ndb_dir.path().to_str().unwrap(), &test_config()).unwrap();
        assert!(
            ndb.add_key(&test_secret_key()),
            "ndb must accept the PNS key"
        );
        let mut test = Self::build(data_path, ndb);
        test._ndb_dir = Some(ndb_dir);
        test
    }

    /// A Dave over a store the caller already has, which must hold the
    /// account's PNS key: the restart of a host that kept its store.
    pub fn over(ndb: Ndb) -> Self {
        let data_dir = TempDir::new().unwrap();
        let mut test = Self::build(&DataPath::new(data_dir.path()), ndb);
        test._data_dir = Some(data_dir);
        test
    }

    /// Add one agentic session per `cwds` entry on `backend`, the first
    /// active, none asking for input focus. Returns their ids in order.
    pub fn add_sessions<const N: usize>(
        &mut self,
        cwds: [&str; N],
        backend: BackendType,
    ) -> [SessionId; N] {
        let manager = &mut self.dave.session_manager;
        let sessions =
            cwds.map(|cwd| manager.new_session(PathBuf::from(cwd), AiMode::Agentic, backend));
        if let Some(&first) = sessions.first() {
            manager.switch_to(first);
        }
        for id in sessions {
            manager.get_mut(id).unwrap().focus_requested = false;
        }
        sessions
    }

    /// A Dave for `ctx`'s host to run: over its store, waker and data dir,
    /// already settled on its selected account, so the first frame the host
    /// runs keeps the sessions a test seeded rather than swapping in the
    /// account's own (empty) ones. The account must hold its secret key.
    ///
    /// Seed it, then hand `test.dave` to the host.
    pub fn hosted(ctx: &mut AppContext<'_>) -> Self {
        let keypair = ctx.accounts.get_selected_account().keypair();
        let sk = keypair
            .secret_key
            .expect("a hosted TestDave signs as the host's account")
            .secret_bytes();
        let account = *keypair.pubkey;
        Self::build_as(ctx.path, ctx.ndb.clone(), sk, account, ctx.waker.clone())
    }

    /// The session `id`, to seed what the methods below don't (its chat,
    /// say).
    pub fn session_mut(&mut self, id: SessionId) -> &mut ChatSession {
        self.dave
            .session_manager
            .get_mut(id)
            .expect("a session the test added")
    }

    /// Have the session `id` work `issue` (a `headway:` card ref), as
    /// `agentium spawn --issue-url` records it.
    pub fn set_issue(&mut self, id: SessionId, issue: &str) {
        self.session_mut(id).details.issue_url = Some(issue.to_owned());
    }

    /// Give the session `parent` a running subagent, as its turn raises one:
    /// a block in its chat, and the subagent's own session beside it.
    pub fn add_subagent(&mut self, parent: SessionId, subagent: SubagentSeed<'_>) {
        self.session_mut(parent)
            .insert_turn_content(Message::Subagent(SubagentInfo {
                task_id: subagent.task_id.into(),
                description: subagent.description.into(),
                subagent_type: subagent.subagent_type.into(),
                status: SubagentStatus::Running,
                output: String::new(),
                max_output_size: 4000,
                tool_results: Vec::new(),
                background: false,
                agent_id: None,
                transcript: Default::default(),
            }));
    }

    /// Open the active session's subagent `task_id`, as its block's `open`
    /// does in the chat: its session becomes active, and the route returned
    /// is the entry the chrome pushes.
    pub fn open_subagent(&mut self, task_id: &str) -> Rc<dyn Any> {
        let dave = &mut self.dave;
        let result = ui::handle_ui_action(
            DaveAction::OpenSubagent(task_id.into()),
            &mut dave.session_manager,
            &RemoteOnlyBackend,
            &mut DaveOverlay::None,
            &mut false,
            &egui::Context::default(),
        );
        let UiActionResult::OpenedSubagent(route) = result else {
            panic!("opening a subagent is a nav entry");
        };
        Rc::new(route)
    }

    fn build(data_path: &DataPath, ndb: Ndb) -> Self {
        let sk = test_secret_key();
        let account = nostrdb_net::FullKeypair::from_secret_bytes(&sk)
            .unwrap()
            .pubkey;
        Self::build_as(data_path, ndb, sk, account, Waker::noop())
    }

    /// A Dave signing as `sk`, whose public key is `account`.
    fn build_as(
        data_path: &DataPath,
        ndb: Ndb,
        sk: [u8; 32],
        account: nostrdb_net::Pubkey,
        waker: Waker,
    ) -> Self {
        seed_agentic_settings(data_path);
        let mut dave = Dave::new(None, ndb.clone(), waker.clone(), data_path);
        // An empty agentic Dave opens on the directory picker; a test picks
        // its sessions itself.
        dave.active_overlay = DaveOverlay::None;
        dave.pns_local_state = Some(PnsLocalState {
            account,
            has_secret_key: true,
        });
        TestDave {
            dave,
            ndb,
            sk,
            account,
            waker,
            _data_dir: None,
            _ndb_dir: None,
        }
    }

    /// Dave and what the app's update loop hands its per-frame work, at
    /// once: the host borrows the store, so it can't be had from `&self`
    /// beside a `&mut Dave`.
    pub fn parts(&mut self) -> TestParts<'_> {
        TestParts {
            host: HostCtx::signing(&self.ndb, &self.sk, &self.waker),
            dave: &mut self.dave,
        }
    }

    /// Create the shared conversation subscription the way
    /// `ensure_pns_local_state` does: notes stored from here on come through
    /// the poll, earlier ones never do.
    pub fn subscribe_feed(&mut self) {
        let sub = subscribe_conversation_events(&self.ndb, self.account).unwrap();
        self.dave.conversation_feed = Some(ConversationFeed::new(sub));
    }

    /// Restore the account's sessions from the store, as startup does:
    /// subscribe the feed if nothing has, run the background worker to the
    /// end, and drain everything it sent. Returns the remote messages the
    /// drain hands on for dispatch.
    pub async fn restore(&mut self) -> Vec<SessionMessage> {
        if self.dave.conversation_feed.is_none() {
            self.subscribe_feed();
        }
        let waker = self.dave.run_restore_worker(&self.ndb, self.account).await;
        self.drain_restore(&waker)
    }

    /// Drain everything the restore worker sent, over as many frames as
    /// `update` would take. One drain stops at its frame budget, which a
    /// loaded machine can spend on the first message alone, so a test that
    /// drained once saw only part of the restore.
    pub fn drain_restore(&mut self, waker: &Waker) -> Vec<SessionMessage> {
        let host = HostCtx::signing(&self.ndb, &self.sk, waker);
        let mut replayed = self.dave.drain_session_restore(&host);
        while !self.dave.session_restore_loader.is_drained() {
            replayed.extend(self.dave.drain_session_restore(&host));
        }
        replayed
    }
}

/// A subagent for [`TestDave::add_subagent`] to raise, by what its block
/// shows.
pub struct SubagentSeed<'a> {
    /// The tool call that started it, which names it to `open_subagent`.
    pub task_id: &'a str,
    /// What it was asked to do.
    pub description: &'a str,
    /// Its kind, e.g. `"Explore"`.
    pub subagent_type: &'a str,
}

/// Pin the constructed [`Dave`] to [`AiMode::Agentic`], which every test
/// assumes — the session manager starts empty there.
///
/// Without this the mode is whatever the machine running the test happens to
/// have installed. `Dave::new` falls back to `ModelConfig::default()` when no
/// settings are saved, and that is
/// `from_env(env, has_binary_on_path("claude"), has_binary_on_path("codex"))`:
/// a dev box with the claude CLI resolves to `BackendType::Claude` and so
/// Agentic, while CI — where neither CLI is on PATH — falls through to OpenAI
/// and so `AiMode::Chat`, which *creates a default session immediately*. That
/// phantom session shifts every session count by one, which is exactly how
/// these tests passed locally and failed in CI.
///
/// Writing settings first takes `Dave::new`'s saved-settings branch instead,
/// and `AiProvider::Anthropic` maps to `BackendType::Claude` (see
/// `ModelConfig::from_settings`), so the mode is the same everywhere.
fn seed_agentic_settings(data_path: &DataPath) {
    let mut settings = TimedSerializer::<DaveSettings>::new(
        data_path,
        DataPathType::Setting,
        "dave_settings.json".to_owned(),
    )
    .with_delay(Duration::ZERO);
    assert!(
        settings.try_save(DaveSettings::with_provider(AiProvider::Anthropic)),
        "seed dave settings so the test runs in a deterministic AiMode"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh fixture is the Dave tests expect: nothing open, no sessions,
    /// the account known.
    #[test]
    fn test_dave_starts_settled() {
        let test = TestDave::new();
        assert!(matches!(test.dave.active_overlay, DaveOverlay::None));
        assert_eq!(test.dave.session_manager.iter().count(), 0);
        assert_eq!(
            test.dave.pns_local_state.as_ref().map(|s| s.account),
            Some(test.account)
        );
    }

    /// The sessions come back in order, the first active, none grabbing the
    /// input's focus.
    #[test]
    fn add_sessions_activates_the_first() {
        let mut test = TestDave::new();
        let [a, b] = test.add_sessions(["/tmp/a", "/tmp/b"], BackendType::Remote);
        let manager = &test.dave.session_manager;
        assert_eq!(manager.active_id(), Some(a));
        assert!(!manager.get(a).unwrap().focus_requested);
        assert!(!manager.get(b).unwrap().focus_requested);
    }
}
