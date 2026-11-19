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
use crate::session::{ChatSession, SessionId, SessionManager, TurnSeq};
use crate::stream_events::HostCtx;
use crate::{embedded_engine, Message};
use agentium_core::session_events::SessionRelation;
use agentium_core::session_loader::TurnSpan;
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
    /// The turn this reports: the session's
    /// [`reported_turn`](crate::session::AgenticSessionData::reported_turn)
    /// once it is sent.
    turn: TurnSeq,
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
/// wakes it and finishes), once per turn, and only when the chat's last
/// assistant text was written after the last reported turn ended, so a turn
/// that wrote none never re-sends the one before.
pub(crate) fn child_report(session: &ChatSession, at_rest: bool) -> Option<ChildReport> {
    if !at_rest || session.is_remote() || session.details.relation != SessionRelation::Child {
        return None;
    }
    let parent = session.details.parent_session.as_deref()?;
    let agentic = session.agentic.as_ref()?;
    if session.status() != AgentStatus::Done {
        return None;
    }

    let turn = session.turn_seq();
    if turn == TurnSeq::NONE || agentic.reported_turn >= Some(turn) {
        return None;
    }
    let (row, text) = session
        .chat
        .iter()
        .enumerate()
        .rev()
        .find_map(|(row, m)| match m {
            Message::Assistant(msg) => Some((row, msg.text().trim())),
            _ => None,
        })?;
    let floor = reported_turn_end(session.turns(), turn, agentic.reported_turn);
    if text.is_empty() || floor.is_some_and(|floor| row <= floor) {
        return None;
    }

    let from = session_ref(agentic.event_session_id());
    let body = report_body(session.details.display_title(), &from, text);
    Some(ChildReport {
        parent: parent.to_string(),
        from,
        body,
        turn,
    })
}

/// The chat row the last reported turn ended on, which a new report's text
/// must come after; `None` when nothing was reported yet.
///
/// Called once `turn` has ended, so its span is the last of `turns`, and each
/// turn between it and the `reported` one ended with a span before it: the
/// reported turn's is that many back. Spans don't carry their seq yet, so a
/// chat replace that folded fewer spans than this host ended loses the anchor
/// (`None`), and the report falls back to the chat's last text.
fn reported_turn_end(
    turns: &[TurnSpan],
    turn: TurnSeq,
    reported: Option<TurnSeq>,
) -> Option<usize> {
    let back = usize::try_from(turn.turns_since(reported?)?).ok()?;
    let index = turns.len().checked_sub(1)?.checked_sub(back)?;
    Some(turns[index].last_row)
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
                agentic.reported_turn = Some(report.turn);
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
    use crate::messages::{AssistantMessage, CompactionInfo, UserMessage};
    use crate::tests::TestDave;
    use std::path::PathBuf;
    use std::time::Duration;

    const PARENT: &str = "agentium:loud-review-oyster";

    /// Run one turn on `session` the way the host does: dispatch `ask`, write
    /// `reply`, end the turn.
    fn run_turn(session: &mut ChatSession, ask: &str, reply: Message) {
        session.chat.push(Message::User(UserMessage::from(ask)));
        session.mark_dispatched();
        session.chat.push(reply);
        session
            .end_turn(crate::session::now_unix())
            .expect("a turn ran");
        session.dispatch_state.stream_ended();
        session.update_status();
    }

    /// An assistant row saying `text`.
    fn says(text: &str) -> Message {
        Message::Assistant(AssistantMessage::from_text(text.to_string()))
    }

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
        run_turn(&mut session, ask, says(answer));
        session
    }

    /// Record `report` as sent, as [`report_child_turns`] does.
    fn sent(child: &mut ChatSession, report: &ChildReport) {
        child.agentic.as_mut().unwrap().reported_turn = Some(report.turn);
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

    /// A reported turn is not sent again, and the next turn reports its own
    /// text.
    #[test]
    fn a_reported_turn_is_not_sent_twice() {
        let mut child = finished(SessionRelation::Child, "find it", "found it");
        let report = child_report(&child, true).unwrap();
        assert_eq!(report.turn, child.turn_seq());
        sent(&mut child, &report);
        assert_eq!(child_report(&child, true), None);

        run_turn(&mut child, "again", says("found more"));
        let next = child_report(&child, true).expect("the new turn reports");
        assert!(next.body.ends_with("found more"), "{}", next.body);
        assert!(next.turn > report.turn);
    }

    /// A turn that wrote no assistant text sends nothing: the last text in
    /// the chat is the reported turn's.
    #[test]
    fn a_textless_turn_sends_nothing() {
        let mut child = finished(SessionRelation::Child, "find it", "found it");
        let report = child_report(&child, true).unwrap();
        sent(&mut child, &report);

        let compacted = Message::CompactionComplete(CompactionInfo { pre_tokens: 0 });
        run_turn(&mut child, "compact", compacted);
        assert_eq!(child.status(), AgentStatus::Done);
        assert_eq!(child_report(&child, true), None);
    }

    /// A turn whose report never went (it ended waiting on a background
    /// task, or its parent was out of reach) is reported by the next turn to
    /// end at rest, even one that wrote no text of its own.
    #[test]
    fn an_unreported_turn_reports_with_the_next() {
        let mut child = finished(SessionRelation::Child, "find it", "found it");
        let compacted = Message::CompactionComplete(CompactionInfo { pre_tokens: 0 });
        run_turn(&mut child, "compact", compacted);
        let report = child_report(&child, true).expect("the first turn's text");
        assert!(report.body.ends_with("found it"), "{}", report.body);
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
            run_turn(session, "find it", says("found it"));
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
