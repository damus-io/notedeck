//! A child session reports each finished turn back to the session that
//! spawned it.
//!
//! A session spawned with `agentium spawn --child` carries its spawner as
//! [`parent_session`](crate::session::SessionDetails::parent_session) and
//! [`SessionRelation::Child`] as its relation. When one of its turns ends and
//! it is at rest — the turn is done, nothing is queued behind it and nothing
//! it started will resume it — the turn's final assistant text goes to the
//! parent as an ordinary message stamped with the child's `agentium:` ref as
//! its `from-session`, exactly like `agentium send` from inside the child. The
//! parent wakes on it like any message; nothing ever blocks on a child.

use crate::agent_status::AgentStatus;
use crate::session::{ChatSession, SessionId, SessionManager};
use crate::stream_events::HostCtx;
use crate::{embedded_engine, Message};
use agentium_core::session_events::SessionRelation;
use agentium_core::wordid::session_ref;
use std::collections::HashSet;

/// The most characters of the turn's text a report carries; the rest stays in
/// the child's own conversation.
const MAX_REPORT_CHARS: usize = 4000;

/// What one finished child turn sends its parent.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ChildReport {
    /// The parent's `agentium:` ref, as the spawn recorded it.
    pub(crate) parent: String,
    /// The child's own `agentium:` ref, stamped as the message's
    /// `from-session`.
    pub(crate) from: String,
    /// The message: a `result from <title>:` line, then the turn's final
    /// assistant text, truncated at [`MAX_REPORT_CHARS`].
    pub(crate) body: String,
    /// The child's assistant row count once this is sent: the session's
    /// [`reported_assistant_rows`](crate::session::AgenticSessionData::reported_assistant_rows)
    /// after the send.
    mark: usize,
}

/// The report `session`'s turn that just ended sends its parent, or `None`
/// when it sends nothing.
///
/// `at_rest` says the turn is really over: no queued message or tool
/// response is about to dispatch, and no compaction will follow. A child
/// mid-conversation would otherwise report every step to its parent.
///
/// Only a local child whose status has settled on [`AgentStatus::Done`]
/// reports (a turn that is waiting on a background task reports when that
/// wakes it and finishes), and only when it wrote assistant text since its
/// last report, so a turn that wrote none never re-sends the one before.
pub(crate) fn child_report(session: &ChatSession, at_rest: bool) -> Option<ChildReport> {
    if !at_rest || session.is_remote() || session.details.relation != SessionRelation::Child {
        return None;
    }
    let parent = session.details.parent_session.as_deref()?;
    let agentic = session.agentic.as_ref()?;
    if session.status() != AgentStatus::Done {
        return None;
    }

    let mut assistants = session.chat.iter().filter_map(|m| match m {
        Message::Assistant(msg) => Some(msg.text()),
        _ => None,
    });
    let mark = assistants.clone().count();
    if mark <= agentic.reported_assistant_rows {
        return None;
    }
    let text = assistants.next_back()?.trim();
    if text.is_empty() {
        return None;
    }

    let from = session_ref(agentic.event_session_id());
    let body = report_body(session.details.display_title(), &from, text);
    Some(ChildReport {
        parent: parent.to_string(),
        from,
        body,
        mark,
    })
}

/// A report's message: one `result from <title>:` line, then `text`, cut at
/// [`MAX_REPORT_CHARS`] with a pointer to `from` for the rest.
fn report_body(title: &str, from: &str, text: &str) -> String {
    let cut = text.char_indices().nth(MAX_REPORT_CHARS).map(|(i, _)| i);
    match cut {
        None => format!("result from {title}:\n{text}"),
        Some(cut) => format!(
            "result from {title}:\n{}… (truncated; {from} for the rest)",
            &text[..cut]
        ),
    }
}

/// Send each ended child turn's report to its parent (see [`child_report`]).
///
/// `ended` are the sessions whose turn ended this drain; one in `needs_send`
/// or `needs_compact` is about to dispatch again, so it is not at rest. The
/// parent is found among the sessions this host knows, local or remote, by
/// its `agentium:` ref, and the message goes through the embedded engine's
/// [`send_message`](agentium_core::Engine::send_message), as `agentium send`
/// does: it is ingested here and the host fans it out, so a parent on
/// another host gets it over the relay.
pub(crate) fn report_child_turns(
    sessions: &mut SessionManager,
    ended: &[SessionId],
    needs_send: &HashSet<SessionId>,
    needs_compact: &HashSet<SessionId>,
    host: &HostCtx<'_>,
) {
    for &session_id in ended {
        let at_rest = !needs_send.contains(&session_id) && !needs_compact.contains(&session_id);
        let Some(session) = sessions.get_mut(session_id) else {
            continue;
        };
        session.update_status();
        let Some(report) = child_report(session, at_rest) else {
            continue;
        };
        if send_report(sessions, &report, host) {
            if let Some(agentic) = sessions
                .get_mut(session_id)
                .and_then(|s| s.agentic.as_mut())
            {
                agentic.reported_assistant_rows = report.mark;
            }
        }
    }
}

/// Send `report` to its parent. Returns whether it went; a parent this host
/// doesn't know, or an account with no secret key, is logged and skipped.
fn send_report(sessions: &SessionManager, report: &ChildReport, host: &HostCtx<'_>) -> bool {
    let parent_id = sessions
        .iter()
        .filter_map(|s| s.agentic.as_ref())
        .map(|a| a.event_session_id())
        .find(|id| session_ref(id) == report.parent);
    let Some(parent_id) = parent_id else {
        tracing::warn!(
            "{}: parent {} is not a session this host knows; result not sent",
            report.from,
            report.parent
        );
        return false;
    };
    let Some(engine) = host.sk().and_then(|sk| embedded_engine(host.ndb, sk)) else {
        return false;
    };
    let opts = agentium_core::SendOptions {
        from_session: Some(&report.from),
    };
    match engine.send_message(parent_id, &report.body, opts) {
        Ok(_) => {
            tracing::info!("{}: sent its turn result to {}", report.from, report.parent);
            true
        }
        Err(e) => {
            tracing::warn!("{}: failed to send its result: {:?}", report.from, e);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendType;
    use crate::config::AiMode;
    use crate::messages::{AssistantMessage, UserMessage};
    use crate::tests::TestDave;
    use std::path::PathBuf;
    use std::time::Duration;

    const PARENT: &str = "agentium:loud-review-oyster";

    /// A local agentic session related to [`PARENT`] by `relation`, whose
    /// turn was asked `ask` and answered `answer`.
    fn finished(relation: SessionRelation, ask: &str, answer: &str) -> ChatSession {
        let mut session = ChatSession::new(
            1,
            PathBuf::from("/tmp"),
            AiMode::Agentic,
            BackendType::Claude,
        );
        session.details.title = "child work".to_string();
        session.details.parent_session = Some(PARENT.to_string());
        session.details.relation = relation;
        session.chat.push(Message::User(UserMessage::from(ask)));
        session
            .chat
            .push(Message::Assistant(AssistantMessage::from_text(
                answer.to_string(),
            )));
        session.update_status();
        session
    }

    /// The trigger: a child at rest with fresh text reports; queued work
    /// behind it, or any other relation, does not.
    #[test]
    fn only_a_child_at_rest_reports() {
        let child = finished(SessionRelation::Child, "find it", "found it");
        let report = child_report(&child, true).expect("a child at rest reports");
        assert_eq!(report.parent, PARENT);
        assert_eq!(
            report.from,
            session_ref(child.agentic.as_ref().unwrap().event_session_id())
        );
        assert_eq!(report.body, "result from child work:\nfound it");

        assert_eq!(child_report(&child, false), None, "queued: not at rest");
        for relation in [SessionRelation::None, SessionRelation::Successor] {
            let other = finished(relation, "find it", "found it");
            assert_eq!(
                child_report(&other, true),
                None,
                "{relation:?} never reports"
            );
        }
    }

    /// A message queued behind the answer leaves the child Idle, not Done,
    /// so it does not report even if the caller thought it at rest.
    #[test]
    fn a_queued_message_holds_the_report() {
        let mut child = finished(SessionRelation::Child, "find it", "found it");
        child
            .chat
            .push(Message::User(UserMessage::from("and then?")));
        child.update_status();
        assert_eq!(child_report(&child, true), None);
    }

    /// Once a turn's text is reported, a turn that ends without writing any
    /// new text does not send the old one again.
    #[test]
    fn a_reported_turn_is_not_sent_twice() {
        let mut child = finished(SessionRelation::Child, "find it", "found it");
        let report = child_report(&child, true).unwrap();
        child.agentic.as_mut().unwrap().reported_assistant_rows = report.mark;
        assert_eq!(child_report(&child, true), None);

        child.chat.push(Message::User(UserMessage::from("again")));
        child
            .chat
            .push(Message::Assistant(AssistantMessage::from_text(
                "found more".to_string(),
            )));
        child.update_status();
        let next = child_report(&child, true).expect("the new turn reports");
        assert!(next.body.ends_with("found more"), "{}", next.body);
    }

    /// A long result is cut at [`MAX_REPORT_CHARS`] characters, on a char
    /// boundary, and points at the child for the rest.
    #[test]
    fn a_long_result_is_truncated() {
        let long = "é".repeat(MAX_REPORT_CHARS + 10);
        let child = finished(SessionRelation::Child, "write a lot", &long);
        let report = child_report(&child, true).unwrap();
        let tail = format!("… (truncated; {} for the rest)", report.from);
        assert!(report.body.ends_with(&tail), "{}", report.body);
        let kept = report.body.strip_suffix(&tail).unwrap();
        let kept = kept.strip_prefix("result from child work:\n").unwrap();
        assert_eq!(kept.chars().count(), MAX_REPORT_CHARS);
    }

    /// End to end on one host: a child's finished turn lands in its parent's
    /// chat once, as a message from the child, and asks the parent to
    /// dispatch it.
    #[tokio::test]
    async fn the_parent_receives_the_childs_result() {
        let mut test = TestDave::new();
        test.subscribe_feed();
        let [parent, child] = test.add_sessions(["/tmp/parent", "/tmp/child"], BackendType::Claude);
        let manager = &mut test.dave.session_manager;
        let parent_ref = session_ref(manager.get(parent).unwrap().agentic().event_session_id());
        let child_ref = session_ref(manager.get(child).unwrap().agentic().event_session_id());
        {
            let session = manager.get_mut(child).unwrap();
            session.details.title = "child work".to_string();
            session.details.parent_session = Some(parent_ref);
            session.details.relation = SessionRelation::Child;
            session
                .chat
                .push(Message::User(UserMessage::from("find it")));
            session
                .chat
                .push(Message::Assistant(AssistantMessage::from_text(
                    "found it".to_string(),
                )));
        }

        let parts = test.parts();
        let none = HashSet::new();
        report_child_turns(
            &mut parts.dave.session_manager,
            &[child],
            &none,
            &none,
            &parts.host,
        );
        // A second drain ending the same turn sends nothing more.
        report_child_turns(
            &mut parts.dave.session_manager,
            &[child],
            &none,
            &none,
            &parts.host,
        );

        let mut dispatch = Vec::new();
        for _ in 0..200 {
            let parts = test.parts();
            dispatch.extend(
                parts
                    .dave
                    .poll_remote_conversation_events(&parts.host, &mut Vec::new()),
            );
            if !dispatch.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Give a duplicate the time to arrive, had one been sent.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let parts = test.parts();
        dispatch.extend(
            parts
                .dave
                .poll_remote_conversation_events(&parts.host, &mut Vec::new()),
        );

        assert_eq!(dispatch.len(), 1, "one message, for the parent");
        assert_eq!(dispatch[0].session_id, parent);
        let users: Vec<&UserMessage> = test
            .dave
            .session_manager
            .get(parent)
            .unwrap()
            .chat
            .iter()
            .filter_map(|m| match m {
                Message::User(user) => Some(user),
                _ => None,
            })
            .collect();
        assert_eq!(users.len(), 1, "{users:?}");
        assert_eq!(users[0].from_session.as_deref(), Some(child_ref.as_str()));
        assert_eq!(users[0].as_str(), "result from child work:\nfound it");
    }
}
