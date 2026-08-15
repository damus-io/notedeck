//! Platform-neutral session runtime state.
//!
//! Holds the egui-free session state types extracted from `notedeck_dave`'s
//! `session.rs`. Currently just [`PermissionTracker`]; further session state
//! joins this module as it is made platform-neutral.

use crate::messages::{
    AnswerSummary, Message, PermissionDecision, PermissionResponse, PermissionResponseType,
};
use std::collections::HashMap;
use tokio::sync::oneshot;
use uuid::Uuid;

/// Consolidated permission tracking for a session.
///
/// Bundles the local oneshot channels (for local sessions), the note-ID
/// mapping (for linking relay responses), and the already-responded set
/// (for remote sessions) into a single struct.
pub struct PermissionTracker {
    /// Local oneshot senders waiting for the user to allow/deny.
    pub pending: HashMap<Uuid, oneshot::Sender<PermissionResponse>>,
    /// Maps permission-request UUID → nostr note ID of the published request.
    pub request_note_ids: HashMap<Uuid, [u8; 32]>,
    /// Permission UUIDs that have already been responded to, with the decision
    /// and its provenance (auto-accepted vs explicit user click). The
    /// auto-accept provenance is persisted in the response event (`"auto"`) and
    /// reconstructed on load, so a remote session rebuilt purely from ndb keeps
    /// the expanded-by-default behaviour for auto-accepted rows.
    pub responded: HashMap<Uuid, PermissionDecision>,
}

impl PermissionTracker {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            request_note_ids: HashMap::new(),
            responded: HashMap::new(),
        }
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
            if let Some(sender) = self.pending.remove(&request_id) {
                if sender.send(response).is_err() {
                    tracing::error!(
                        "failed to send permission response for request {}",
                        request_id
                    );
                }
            } else {
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
