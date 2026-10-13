//! The Permissions card's state: how the session's permission prompts were
//! answered.
//!
//! The source is the chat's `PermissionRequest` rows. A row carries its own
//! response once the session saw it answered, and the session's
//! `PermissionTracker::responded` map holds the decisions reached before the
//! row caught up (a remote response folded in, an allowlist auto-accept), so a
//! row counts as answered when either says so.
//!
//! The counts are a walk over the chat that allocates nothing, so the card
//! takes them every frame it is drawn and builds its strings only when they
//! move ([`PermissionLabels`]).

use super::LabelMemo;
use crate::messages::{PermissionDecision, PermissionResponseType};
use crate::Message;
use std::collections::HashMap;
use uuid::Uuid;

/// How a session's permission prompts were answered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PermissionCounts {
    /// Allowed by a user's click.
    pub allowed: usize,
    /// Denied, or exited, by a user. An exit is recorded as a denial with a
    /// canned reason, so the two can't be told apart here.
    pub denied: usize,
    /// Allowed without a click: the runtime allowlist, or the backend's own
    /// auto-accept rules.
    pub auto_accepted: usize,
    /// Not answered yet, or never: a prompt the turn ended under.
    pub unanswered: usize,
}

impl PermissionCounts {
    /// Count `chat`'s permission prompts, taking a row's decision from the row
    /// or, when the row hasn't caught up, from `responded`.
    pub fn of(chat: &[Message], responded: Option<&HashMap<Uuid, PermissionDecision>>) -> Self {
        let mut counts = Self::default();
        for message in chat {
            let Message::PermissionRequest(req) = message else {
                continue;
            };
            let recorded = responded.and_then(|r| r.get(&req.id));
            let response = req.response.or(recorded.map(|d| d.response));
            let auto = req.auto_accepted || recorded.is_some_and(|d| d.auto_accepted);
            match response {
                None => counts.unanswered += 1,
                Some(PermissionResponseType::Allowed) if auto => counts.auto_accepted += 1,
                Some(PermissionResponseType::Allowed) => counts.allowed += 1,
                Some(PermissionResponseType::Denied) => counts.denied += 1,
            }
        }
        counts
    }

    /// The prompts a user was asked: everything but the auto-accepted.
    pub fn asked(&self) -> usize {
        self.allowed + self.denied + self.unanswered
    }
}

/// The Permissions card's strings, rebuilt when the counts move.
pub type PermissionMemo = LabelMemo<PermissionCounts, PermissionLabels>;

/// The Permissions card's display strings.
#[derive(Debug, Default)]
pub struct PermissionLabels {
    /// How many prompts a user was asked: "9".
    pub asked: String,
    /// How they were answered: "7 allowed · 1 denied · 1 unanswered".
    pub breakdown: String,
    /// How many were accepted without asking: "214".
    pub auto_accepted: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::PermissionRequest;

    fn request(response: Option<PermissionResponseType>) -> PermissionRequest {
        PermissionRequest::new(
            Uuid::new_v4(),
            "Bash".into(),
            serde_json::json!({ "command": "ls" }),
            None,
            response,
            None,
        )
    }

    /// Each prompt lands in one bucket: an auto-accepted allow isn't a
    /// user's allow, and a decision only the tracker holds still counts.
    #[test]
    fn counts_each_prompt_once() {
        let allowed = request(Some(PermissionResponseType::Allowed));
        let denied = request(Some(PermissionResponseType::Denied));
        let auto = request(None).auto_accept();
        let pending = request(None);
        // Answered, but only the tracker knows yet.
        let tracked = request(None);
        let responded = HashMap::from([(
            tracked.id,
            PermissionDecision {
                response: PermissionResponseType::Allowed,
                auto_accepted: true,
            },
        )]);

        let chat: Vec<Message> = [allowed, denied, auto, pending, tracked]
            .into_iter()
            .map(Message::PermissionRequest)
            .chain([Message::System("".into())])
            .collect();

        let counts = PermissionCounts::of(&chat, Some(&responded));
        assert_eq!(
            counts,
            PermissionCounts {
                allowed: 1,
                denied: 1,
                auto_accepted: 2,
                unanswered: 1,
            }
        );
        assert_eq!(counts.asked(), 3);

        // Without the tracker, the last prompt reads as unanswered.
        assert_eq!(PermissionCounts::of(&chat, None).unanswered, 2);
    }
}
