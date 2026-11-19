//! Platform-neutral session runtime state.
//!
//! Holds the egui-free session state types extracted from `notedeck_dave`'s
//! `session.rs`. Currently just [`PermissionTracker`]; further session state
//! joins this module as it is made platform-neutral.

use crate::messages::{
    AnswerSummary, Message, PermissionDecision, PermissionResponse, PermissionResponseType,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::oneshot;
use uuid::Uuid;

/// Consolidated permission tracking for a session.
///
/// Bundles the local oneshot channels (for local sessions), the note-ID
/// mapping (for linking relay responses), and the already-responded set
/// (for remote sessions) into a single struct.
pub struct PermissionTracker {
    /// Local oneshot senders waiting for the user to allow/deny. Private so
    /// that every answer goes through [`answer`](Self::answer) (and its wake);
    /// add one with [`ask`](Self::ask).
    pending: HashMap<Uuid, oneshot::Sender<PermissionResponse>>,
    /// Maps permission-request UUID → nostr note ID of the published request.
    pub request_note_ids: HashMap<Uuid, [u8; 32]>,
    /// Permission UUIDs that have already been responded to, with the decision
    /// and its provenance (auto-accepted vs explicit user click). The
    /// auto-accept provenance is persisted in the response event (`"auto"`) and
    /// reconstructed on load, so a remote session rebuilt purely from ndb keeps
    /// the expanded-by-default behaviour for auto-accepted rows.
    pub responded: HashMap<Uuid, PermissionDecision>,
    /// Asks the host for another pass once a local request is answered, for
    /// a waiter that reads its answer on the host's own pass rather than
    /// awaiting it (see [`wake_on_answer`](Self::wake_on_answer)).
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl PermissionTracker {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            request_note_ids: HashMap::new(),
            responded: HashMap::new(),
            wake: None,
        }
    }

    /// Call `wake` each time a local request is answered
    /// ([`answer`](Self::answer)).
    ///
    /// A backend that awaits its oneshot is woken by the send itself. A
    /// waiter that polls its receiver once per host pass, as Dave's native
    /// tool loop does, would otherwise read the answer only when something
    /// else happened to bring the next pass. Replaces any earlier callback.
    pub fn wake_on_answer(&mut self, wake: impl Fn() + Send + Sync + 'static) {
        self.wake = Some(Arc::new(wake));
    }

    /// Register the local request `id`, whose waiter reads `tx`'s receiver.
    /// It stays pending until [`answer`](Self::answer)ed or
    /// [`withdraw`](Self::withdraw)n. Asking again with the same `id`
    /// replaces the earlier sender, dropping its waiter.
    pub fn ask(&mut self, id: Uuid, tx: oneshot::Sender<PermissionResponse>) {
        self.pending.insert(id, tx);
    }

    /// Whether the local request `id` is still waiting for an answer.
    pub fn is_pending(&self, id: Uuid) -> bool {
        self.pending.contains_key(&id)
    }

    /// Any one pending local request, or `None` if none is. Which one is
    /// unspecified when several are pending; a caller that needs a
    /// particular order picks from its chat instead.
    pub fn first_pending(&self) -> Option<Uuid> {
        self.pending.keys().next().copied()
    }

    /// How many local requests are waiting for an answer.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Send `response` to the local request `request_id`'s waiter and wake
    /// the host (see [`wake_on_answer`](Self::wake_on_answer)). Returns
    /// whether the request was still pending: only the first answer counts.
    pub fn answer(&mut self, request_id: Uuid, response: PermissionResponse) -> bool {
        let Some(sender) = self.pending.remove(&request_id) else {
            return false;
        };
        if sender.send(response).is_err() {
            tracing::error!(
                "failed to send permission response for request {}",
                request_id
            );
        }
        if let Some(wake) = &self.wake {
            wake();
        }
        true
    }

    /// Withdraw the local request `request_id`, whose waiter has gone (the
    /// turn that asked was stopped): drop its sender so it no longer counts
    /// as pending, and mark its chat row denied so nothing offers to answer
    /// it. Returns whether it was still pending; an answered request is left
    /// as it is.
    pub fn withdraw(&mut self, chat: &mut [Message], request_id: Uuid) -> bool {
        if self.pending.remove(&request_id).is_none() {
            return false;
        }
        let row = chat.iter_mut().find_map(|msg| match msg {
            Message::PermissionRequest(req) if req.id == request_id => Some(req),
            _ => None,
        });
        if let Some(req) = row {
            req.response.get_or_insert(PermissionResponseType::Denied);
        }
        true
    }

    /// Whether there are unresolved local permission requests.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Resolve a permission request. This is the ONLY place resolution state
    /// is updated — both `handle_permission_response` and
    /// `handle_question_response` funnel through here.
    pub fn resolve(
        &mut self,
        chat: &mut [Message],
        request_id: Uuid,
        response_type: PermissionResponseType,
        answer_summary: Option<AnswerSummary>,
        is_remote: bool,
        oneshot_response: Option<PermissionResponse>,
    ) {
        // 1. Update the PermissionRequest message in chat
        for msg in chat.iter_mut() {
            if let Message::PermissionRequest(req) = msg {
                if req.id == request_id {
                    req.response = Some(response_type);
                    if answer_summary.is_some() {
                        req.answer_summary = answer_summary;
                    }
                    break;
                }
            }
        }

        // 2. Update PermissionTracker state. This path is an explicit user
        // decision (allow/deny click), so it is never auto-accepted.
        if is_remote {
            self.responded.insert(
                request_id,
                PermissionDecision {
                    response: response_type,
                    auto_accepted: false,
                },
            );
        } else if let Some(response) = oneshot_response {
            if !self.answer(request_id, response) {
                tracing::warn!("no pending permission found for request {}", request_id);
            }
        }
    }

    /// Merge loaded permission state from restored events.
    ///
    /// Every persisted field is *merged* (not replaced) so in-memory decisions
    /// survive a reload: an auto-accept recorded this poll whose response event
    /// hasn't yet round-tripped into ndb is kept, while persisted decisions from
    /// ndb win on conflict (they are authoritative). At initial load the loaded
    /// maps are empty, so this is equivalent to assignment.
    ///
    /// Takes the whole loaded [`PermissionTracker`] rather than individual fields
    /// so a newly added persisted field can't be silently dropped here — the
    /// auto-accept provenance folded into [`responded`](Self::responded) went
    /// missing on the remote path exactly because a field-by-field reconstruction
    /// stranded it. `loaded.pending` is always empty from the loader.
    pub fn merge_loaded(&mut self, loaded: PermissionTracker) {
        self.responded.extend(loaded.responded);
        self.request_note_ids.extend(loaded.request_note_ids);
    }
}

impl Default for PermissionTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::PermissionRequest;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A tracker with one pending request, `id`, and the request's waiter.
    struct Asked {
        tracker: PermissionTracker,
        id: Uuid,
        rx: oneshot::Receiver<PermissionResponse>,
    }

    /// [`Asked`], its request shown in `chat`.
    fn asked(chat: &mut Vec<Message>) -> Asked {
        let mut tracker = PermissionTracker::new();
        let id = Uuid::new_v4();
        let (tx, rx) = oneshot::channel();
        tracker.ask(id, tx);
        chat.push(Message::PermissionRequest(PermissionRequest::pending(
            id,
            "headway_move_card".into(),
            serde_json::json!({}),
        )));
        Asked { tracker, id, rx }
    }

    /// Answering a request wakes the host once, and only the first answer
    /// counts.
    #[test]
    fn an_answer_wakes_the_host() {
        let mut chat = Vec::new();
        let Asked {
            mut tracker,
            id,
            mut rx,
        } = asked(&mut chat);
        let wakes = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&wakes);
        tracker.wake_on_answer(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });

        tracker.resolve(
            &mut chat,
            id,
            PermissionResponseType::Allowed,
            None,
            false,
            Some(PermissionResponse::Allow { message: None }),
        );
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert!(matches!(
            rx.try_recv(),
            Ok(PermissionResponse::Allow { .. })
        ));
        assert!(!tracker.answer(id, PermissionResponse::Allow { message: None }));
        assert_eq!(wakes.load(Ordering::SeqCst), 1, "no second wake");
    }

    /// A withdrawn request stops counting as pending and its row reads
    /// denied; withdrawing it again changes nothing.
    #[test]
    fn a_withdrawn_request_is_no_longer_pending() {
        let mut chat = Vec::new();
        let Asked {
            mut tracker,
            id,
            rx,
        } = asked(&mut chat);
        drop(rx);

        assert!(tracker.withdraw(&mut chat, id));
        assert!(!tracker.has_pending());
        assert!(matches!(
            &chat[0],
            Message::PermissionRequest(req) if req.response == Some(PermissionResponseType::Denied)
        ));
        assert!(!tracker.withdraw(&mut chat, id));
    }

    /// An asked request is pending, counted and pickable until it is
    /// answered; an id never asked is not pending.
    #[test]
    fn an_asked_request_is_pending_until_answered() {
        let mut chat = Vec::new();
        let Asked {
            mut tracker,
            id,
            rx: _rx,
        } = asked(&mut chat);
        assert!(tracker.is_pending(id));
        assert!(!tracker.is_pending(Uuid::new_v4()));
        assert_eq!(tracker.pending_count(), 1);
        assert_eq!(tracker.first_pending(), Some(id));

        assert!(tracker.answer(id, PermissionResponse::Allow { message: None }));
        assert!(!tracker.is_pending(id));
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.first_pending(), None);
    }
}
