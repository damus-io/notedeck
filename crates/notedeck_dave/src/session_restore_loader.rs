//! Background loader for Dave's startup session restore.
//!
//! Restoring an account's persisted sessions from ndb is expensive: it folds
//! every kind-31988 state event down to the latest revision per session, then
//! for *each* live session queries and renders that session's entire kind-1988
//! conversation history into [`Message`](agentium_core::messages::Message)s. On a
//! large store this is hundreds of thousands of notes — far too much to do on the
//! render thread without freezing the window on load.
//!
//! This mirrors [`notedeck_columns::timeline_loader`] on top of the generic
//! [`AsyncLoader`]: a worker thread does the whole read + render (all producing
//! `Send` owned data — [`SessionState`] and [`LoadedSession`]) and streams one
//! finished session at a time back over a channel. The main thread drains a few
//! per frame under a time budget, doing only the cheap `&mut self`-bound work
//! (creating + hydrating the session), so the first frame paints immediately and
//! the session list fills in progressively.

use std::collections::HashMap;
use std::path::PathBuf;

use crossbeam_channel as chan;
use nostrdb::{Ndb, Transaction};
use nostrdb_net::Pubkey;
use notedeck::{worker_count, AsyncLoader};

use agentium_core::session_loader::{self, LoadedSession, SessionState};

use tracing::info;

/// Upper bound on restore worker threads. Restore is one command per account, so
/// effective parallelism is usually 1; the clamp only matters if several accounts
/// restore at once.
const MAX_SESSION_RESTORE_WORKERS: usize = 4;

/// Request a repaint every this many streamed sessions, so the drain loop keeps
/// getting woken without a repaint storm (one wake per note-batch, matching
/// `timeline_loader`'s flush cadence).
const REPAINT_EVERY: usize = 16;

/// Commands sent to the session-restore worker.
pub enum SessionRestoreCmd {
    /// Restore all persisted sessions for `account` from ndb.
    RestoreAccount {
        /// Selected account whose sessions to restore.
        account: Pubkey,
    },
}

/// Messages streamed back from the session-restore worker.
pub enum SessionRestoreMsg {
    /// Emitted once, right after the (cheap) state fold and *before* the
    /// expensive per-session render. Carries the live-session count (for the
    /// startup overlay decision) and the per-host recent paths (for the directory
    /// picker seed) so the main thread can act before history streams in.
    Started {
        /// Account this restore is for.
        account: Pubkey,
        /// Number of live sessions that will be streamed.
        live_count: usize,
        /// Per-host recent working directories, for the directory-picker seed.
        host_paths: HashMap<String, Vec<PathBuf>>,
    },
    /// One fully-loaded session: its latest state plus rendered conversation
    /// history. Both boxed to keep the enum small ([`LoadedSession`] is large).
    Session {
        /// Account this session belongs to.
        account: Pubkey,
        /// The session's latest kind-31988 state.
        state: Box<SessionState>,
        /// The session's rendered conversation history + dedup/permission state.
        loaded: Box<LoadedSession>,
    },
    /// Restore finished for `account`; `restored` is the number of `Session`
    /// messages that were sent.
    Finished {
        /// Account whose restore finished.
        account: Pubkey,
        /// Count of sessions streamed.
        restored: usize,
    },
    /// Restore failed (e.g. a transaction could not be opened).
    Failed {
        /// Account whose restore failed.
        account: Pubkey,
        /// Human-readable failure reason.
        error: String,
    },
}

/// Handle for driving the session-restore worker thread(s).
pub struct SessionRestoreLoader {
    loader: AsyncLoader<SessionRestoreCmd, SessionRestoreMsg>,
}

impl SessionRestoreLoader {
    /// Create an uninitialized loader handle.
    pub fn new() -> Self {
        Self {
            loader: AsyncLoader::new(),
        }
    }

    /// Start the loader workers if they have not been started yet. Idempotent —
    /// safe to call every frame.
    pub fn start(&mut self, egui_ctx: egui::Context, ndb: Ndb) {
        let workers = worker_count(MAX_SESSION_RESTORE_WORKERS);
        let started = self
            .loader
            .start(egui_ctx, ndb, workers, "dave-session-restore", handle_cmd);
        if started {
            info!(workers, "starting session restore workers");
        }
    }

    /// Request a restore of every persisted session for `account`.
    pub fn restore_account(&self, account: Pubkey) {
        self.loader
            .send(SessionRestoreCmd::RestoreAccount { account });
    }

    /// Try to receive the next loader message without blocking.
    pub fn try_recv(&self) -> Option<SessionRestoreMsg> {
        self.loader.try_recv()
    }
}

impl Default for SessionRestoreLoader {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle a restore command on a worker thread.
fn handle_cmd(
    cmd: SessionRestoreCmd,
    egui_ctx: &egui::Context,
    ndb: &Ndb,
    msg_tx: &chan::Sender<SessionRestoreMsg>,
) {
    match cmd {
        SessionRestoreCmd::RestoreAccount { account } => {
            restore_account(egui_ctx, ndb, msg_tx, account)
        }
    }
}

/// Read + render an account's sessions off the render thread, streaming one
/// finished session at a time. Sends [`SessionRestoreMsg::Failed`] and returns
/// early on a transaction error; otherwise emits `Started`, then a `Session` per
/// live session, then `Finished`.
fn restore_account(
    egui_ctx: &egui::Context,
    ndb: &Ndb,
    msg_tx: &chan::Sender<SessionRestoreMsg>,
    account: Pubkey,
) {
    let txn = match Transaction::new(ndb) {
        Ok(t) => t,
        Err(e) => {
            let _ = msg_tx.send(SessionRestoreMsg::Failed {
                account,
                error: format!("failed to open txn for session restore: {e:?}"),
            });
            egui_ctx.request_repaint();
            return;
        }
    };

    // Cheap fold: latest live state per session. Do the host-paths query here
    // too, while we hold the txn, and hand both to the main thread up front so it
    // can settle the startup overlay before the heavy render streams in.
    let states = session_loader::load_session_states_for_author(ndb, &txn, &account);
    let host_paths = session_loader::load_recent_paths_by_host_for_author(ndb, &txn, &account);

    if msg_tx
        .send(SessionRestoreMsg::Started {
            account,
            live_count: states.len(),
            host_paths,
        })
        .is_err()
    {
        return;
    }
    egui_ctx.request_repaint();

    let mut restored = 0usize;
    for state in states {
        // The expensive part: query + sort + render this session's whole history.
        let loaded = session_loader::load_session_messages_for_author(
            ndb,
            &txn,
            &account,
            &state.claude_session_id,
        );

        if msg_tx
            .send(SessionRestoreMsg::Session {
                account,
                state: Box::new(state),
                loaded: Box::new(loaded),
            })
            .is_err()
        {
            return;
        }
        restored += 1;
        if restored.is_multiple_of(REPAINT_EVERY) {
            egui_ctx.request_repaint();
        }
    }

    let _ = msg_tx.send(SessionRestoreMsg::Finished { account, restored });
    egui_ctx.request_repaint();
}
