//! Reconciling a local session's chat to the fold at rest
//! (headway:dave/barrel-access-ill).
//!
//! Every other view of a session — a remote observer, the host after a
//! restart, the `agentium` CLI — is the fold over its kind-1988 notes. The
//! host builds its live chat from the backend stream instead, because a turn
//! in flight has rows no note describes yet (an open assistant segment, a
//! running tool). Once the turn is at rest every row has been published, so
//! the host swaps its chat for the fold and becomes the same view by
//! construction. [`maybe_reconcile_at_rest`] does the swap, and warns when
//! the chat it replaced differed from the fold: a row the host shows that no
//! note carries (or the reverse) is a publish that was missed.
//!
//! The wire copy of some rows is smaller than the host's own: tool output and
//! diffs are capped, a permission's tool input is truncated, a subagent's
//! output is cut, images aren't sent. [`LocalOverlay`] carries that detail from
//! the old chat onto the fold's rows, matched by id, so the host keeps showing
//! it. None of it is part of a row's [`view_signature`], so the overlay never
//! changes what the drift check compares.

use crate::conversation::rebuild_chat_from_fold;
use crate::session::ChatSession;
use crate::tools::ToolResponses;
use crate::{ExecutedTool, ImageAttachment, Message};
use agentium_core::messages::PermissionRequest;
use agentium_core::session_loader::{view_signature, RowSig};
use nostrdb::Transaction;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a session may sit at rest behind a note nostrdb hasn't handed
/// back before [`warn_stalled_reconciles`] says so. A note indexes within
/// milliseconds, so one still missing after this won't index at all.
const UNINDEXED_STALL_AFTER: Duration = Duration::from_secs(10);

/// What [`maybe_reconcile_at_rest`] did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReconcileOutcome {
    /// The session isn't ready: not at rest, a note it published isn't indexed
    /// yet, or its chat gained no row since the last reconcile (a note it
    /// published, or a remote user message).
    NotReady,
    /// The chat was swapped for the fold and showed the same rows.
    Converged,
    /// The chat was swapped for the fold, which showed different rows.
    Drifted(Drift),
}

/// The first row where the host's chat and the fold disagreed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Drift {
    /// The row's index in both views.
    pub index: usize,
    /// The host's row there, `None` past the end of its chat.
    pub host: Option<RowSig>,
    /// The fold's row there, `None` past the end of the fold.
    pub fold: Option<RowSig>,
}

/// Swap a local session's chat for the fold over its notes, if it is at rest.
///
/// Runs only when all of these hold:
/// - the session is at rest ([`ChatSession::at_rest`]), which includes no
///   user message waiting to be dispatched;
/// - its chat gained a row since its last reconcile (`fold_dirty`);
/// - nostrdb has handed back every note it published
///   (`unindexed_self_notes` is empty), so the fold is complete.
///
/// Called after each conversation poll batch, which is where the last of a
/// turn's notes arrive, and at the end of a turn, for one whose notes were all
/// indexed before it ended. The drift is logged, and returned for tests.
///
/// A note that never indexes keeps the session waiting, so it keeps its live
/// chat rather than lose the row. Today that is any note whose inner event is
/// over 32KB: nostrdb's NIP-44 unpad rejects it. [`warn_stalled_reconciles`]
/// logs a session stuck that way.
///
/// The fold is O(session) and runs on the UI thread, once per turn.
#[profiling::function]
pub(crate) fn maybe_reconcile_at_rest(
    session: &mut ChatSession,
    ndb: &nostrdb::Ndb,
    author: &nostrdb_net::Pubkey,
) -> ReconcileOutcome {
    let ready = session.at_rest()
        && session
            .agentic
            .as_ref()
            .is_some_and(|a| a.fold_dirty && a.unindexed_self_notes.is_empty());
    if !ready {
        return ReconcileOutcome::NotReady;
    }
    let Ok(txn) = Transaction::new(ndb) else {
        return ReconcileOutcome::NotReady;
    };

    let host = view_signature(&session.chat);
    let overlay = LocalOverlay::take(std::mem::take(&mut session.chat));
    rebuild_chat_from_fold(session, ndb, &txn, author);
    overlay.apply(&mut session.chat);
    if let Some(agentic) = &mut session.agentic {
        agentic.fold_dirty = false;
    }

    let Some(drift) = first_drift(&host, &view_signature(&session.chat)) else {
        return ReconcileOutcome::Converged;
    };
    tracing::warn!(
        session = session.id,
        index = drift.index,
        host = ?drift.host,
        fold = ?drift.fold,
        "host view drifted from fold"
    );
    ReconcileOutcome::Drifted(drift)
}

/// Warn once for each local session held off its reconcile by a note that
/// never indexed: at rest, with a note it published still missing from
/// nostrdb after [`UNINDEXED_STALL_AFTER`].
///
/// [`maybe_reconcile_at_rest`] waits for such a note silently, and only runs
/// when a poll or a turn end calls it, so nothing else would notice. Run once
/// a frame; a session with every note indexed costs one emptiness check.
pub(crate) fn warn_stalled_reconciles<'a>(
    sessions: impl Iterator<Item = &'a mut ChatSession>,
    now: Instant,
) {
    for session in sessions {
        let at_rest = session.at_rest();
        let Some(agentic) = &mut session.agentic else {
            continue;
        };
        if !at_rest {
            continue;
        }
        let Some(stall) = agentic.unindexed_self_notes.take_stall(now) else {
            continue;
        };
        tracing::warn!(
            session = session.id,
            unindexed = stall.count,
            oldest_secs = stall.oldest_age.as_secs(),
            "session at rest can't reconcile: a note it published never indexed"
        );
    }
}

/// Kind-1988 notes a host published for a session that nostrdb has not
/// handed back through the conversation subscription yet, each with when it
/// was published.
///
/// The reconcile at rest waits for this to empty, since a fold taken earlier
/// would be missing rows the host is showing. The publish times are what let
/// [`warn_stalled_reconciles`] tell a note on its way from one that never
/// arrives.
#[derive(Debug, Default)]
pub(crate) struct UnindexedNotes {
    published: HashMap<[u8; 32], Instant>,
    /// The stall warning fired for the notes held now. Cleared once they have
    /// all indexed, so a later stall warns again.
    stall_warned: bool,
}

/// What [`UnindexedNotes::take_stall`] reports.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Stall {
    /// Notes still unindexed.
    pub count: usize,
    /// How long ago the oldest of them was published.
    pub oldest_age: Duration,
}

impl UnindexedNotes {
    /// Hold a note just published at `now` until nostrdb hands it back.
    pub fn insert(&mut self, note_id: [u8; 32], now: Instant) {
        self.published.insert(note_id, now);
    }

    /// Release a note nostrdb has handed back.
    pub fn remove(&mut self, note_id: &[u8; 32]) {
        self.published.remove(note_id);
        if self.published.is_empty() {
            self.stall_warned = false;
        }
    }

    /// Every note published has been handed back.
    pub fn is_empty(&self) -> bool {
        self.published.is_empty()
    }

    /// The held notes, once the oldest has waited [`UNINDEXED_STALL_AFTER`].
    /// Reported once, until they have all indexed.
    pub fn take_stall(&mut self, now: Instant) -> Option<Stall> {
        if self.stall_warned {
            return None;
        }
        let oldest = self.published.values().min()?;
        let oldest_age = now.saturating_duration_since(*oldest);
        if oldest_age < UNINDEXED_STALL_AFTER {
            return None;
        }
        self.stall_warned = true;
        Some(Stall {
            count: self.published.len(),
            oldest_age,
        })
    }
}

/// The first index where two views differ, including one running longer.
fn first_drift(host: &[RowSig], fold: &[RowSig]) -> Option<Drift> {
    let index = host
        .iter()
        .zip(fold)
        .position(|(h, f)| h != f)
        .or_else(|| (host.len() != fold.len()).then(|| host.len().min(fold.len())))?;
    Some(Drift {
        index,
        host: host.get(index).cloned(),
        fold: fold.get(index).cloned(),
    })
}

/// Detail the host holds for its rows that their notes carry only in part,
/// taken out of the chat a reconcile replaces and laid over the fold's rows.
///
/// Every entry is keyed by an id the fold's row carries too. A row with no
/// such id (a tool result the backend gave no `tool_use_id`) keeps the fold's
/// copy.
#[derive(Default)]
struct LocalOverlay {
    /// Finished tools by `tool_use_id`, top-level and subagent-internal. The
    /// wire copy's output is cut, and its diff dropped, to fit the wire
    /// budget (`MAX_WIRE_EVENT_BYTES`).
    tools: HashMap<String, ExecutedTool>,
    /// Permission requests by perm id. The wire copy's tool input is
    /// truncated, and a question set's answer summary isn't sent.
    permissions: HashMap<uuid::Uuid, PermissionRequest>,
    /// Subagent output by task id. The wire copy is cut to a budget.
    subagent_output: HashMap<String, String>,
    /// A user message's images by its note id. Images aren't sent.
    images: HashMap<[u8; 32], Vec<ImageAttachment>>,
}

impl LocalOverlay {
    /// Move the detail out of the chat being replaced. It runs once per turn,
    /// at rest, and moves each row's detail rather than cloning it; only a
    /// tool's id is cloned, to key it by.
    fn take(chat: Vec<Message>) -> Self {
        let mut overlay = LocalOverlay::default();
        for message in chat {
            match message {
                Message::ToolResponse(resp) => {
                    if let ToolResponses::ExecutedTool(tool) = resp.into_responses() {
                        overlay.add_tool(tool);
                    }
                }
                Message::Subagent(info) => {
                    for tool in info.tool_results {
                        overlay.add_tool(tool);
                    }
                    overlay.subagent_output.insert(info.task_id, info.output);
                }
                Message::PermissionRequest(req) => {
                    overlay.permissions.insert(req.id, req);
                }
                Message::User(user) if !user.images.is_empty() => {
                    if let Some(note_id) = user.note_id {
                        overlay.images.insert(note_id, user.images);
                    }
                }
                _ => {}
            }
        }
        overlay
    }

    fn add_tool(&mut self, tool: ExecutedTool) {
        if let Some(id) = tool.tool_use_id.clone() {
            self.tools.insert(id, tool);
        }
    }

    /// Lay the detail over the fold's rows that share its ids.
    fn apply(mut self, chat: &mut [Message]) {
        for message in chat.iter_mut() {
            match message {
                Message::ToolResponse(resp) => {
                    if let ToolResponses::ExecutedTool(tool) = resp.responses_mut() {
                        self.restore_tool(tool);
                    }
                }
                Message::Subagent(info) => {
                    for tool in &mut info.tool_results {
                        self.restore_tool(tool);
                    }
                    if let Some(output) = self.subagent_output.remove(&info.task_id) {
                        if output.len() > info.output.len() {
                            info.output = output;
                        }
                    }
                }
                Message::PermissionRequest(req) => {
                    let Some(host) = self.permissions.remove(&req.id) else {
                        continue;
                    };
                    req.tool_input = host.tool_input;
                    req.view = host.view;
                    if req.answer_summary.is_none() {
                        req.answer_summary = host.answer_summary;
                    }
                }
                Message::User(user) => {
                    if let Some(images) = user.note_id.and_then(|id| self.images.remove(&id)) {
                        user.images = images;
                    }
                }
                _ => {}
            }
        }
    }

    /// Put back what the wire cut from a finished tool: output longer than the
    /// note's (the note's is a capped copy of it), and a diff the note dropped
    /// for size.
    fn restore_tool(&mut self, tool: &mut ExecutedTool) {
        let Some(host) = tool
            .tool_use_id
            .as_ref()
            .and_then(|id| self.tools.remove(id))
        else {
            return;
        };
        let len = |output: &Option<String>| output.as_ref().map_or(0, String::len);
        if len(&host.output) > len(&tool.output) {
            tool.output = host.output;
        }
        if tool.file_update.is_none() {
            tool.file_update = host.file_update;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A note still unindexed past the threshold is reported once, with the
    /// count and the oldest's age, and a later stall reports again once every
    /// note has indexed in between.
    #[test]
    fn unindexed_stall_reports_once_per_stall() {
        let start = Instant::now();
        let mut notes = UnindexedNotes::default();
        notes.insert([1; 32], start);
        notes.insert([2; 32], start + Duration::from_secs(4));

        let early = start + UNINDEXED_STALL_AFTER - Duration::from_millis(1);
        assert_eq!(notes.take_stall(early), None, "not stuck yet");

        let late = start + UNINDEXED_STALL_AFTER + Duration::from_secs(1);
        assert_eq!(
            notes.take_stall(late),
            Some(Stall {
                count: 2,
                oldest_age: UNINDEXED_STALL_AFTER + Duration::from_secs(1),
            })
        );
        assert_eq!(notes.take_stall(late), None, "reported once");

        // One indexing leaves the stall standing, still reported.
        notes.remove(&[2; 32]);
        assert_eq!(notes.take_stall(late), None);

        // Once all have indexed, the next stall is a new one.
        notes.remove(&[1; 32]);
        assert!(notes.is_empty());
        notes.insert([3; 32], late);
        let later = late + UNINDEXED_STALL_AFTER;
        assert_eq!(
            notes.take_stall(later),
            Some(Stall {
                count: 1,
                oldest_age: UNINDEXED_STALL_AFTER,
            })
        );
    }
}
