//! The scene map's message lines: which session has messaged which, when
//! last, and whether a packet is still travelling the line for a message
//! that just arrived.
//!
//! A message between sessions is a user message in the receiver's chat
//! whose [`from_session`](crate::messages::UserMessage::from_session) names
//! the sender. [`MessageLines::sync`] rescans a session's chat only when its
//! length changed, and resolves senders to sessions only then or when the
//! map's membership did, so a frame where nothing arrived costs one length
//! check per session.

use std::collections::HashMap;

use crate::messages::Message;
use crate::session::{ChatSession, SessionId, SessionManager};

/// How long a message line stays drawn after its last message, in seconds.
pub const LINE_SECS: f64 = 10.0 * 60.0;
/// The last part of [`LINE_SECS`] over which a line fades out.
pub const FADE_SECS: f64 = 60.0;
/// How long a packet takes to travel a line after a message arrives.
pub const PACKET_SECS: f64 = 3.0;
/// How recently a message must have been sent for its arrival to send a
/// packet. A message older than this that turns up (history being loaded)
/// is not an arrival.
const FRESH_SECS: u64 = 30;

/// Every message line the map draws, kept across frames.
#[derive(Default)]
pub struct MessageLines {
    /// What each session's chat said about who messaged it, at the chat
    /// length it was scanned at.
    heard: HashMap<SessionId, Heard>,
    /// The lines between sessions that are both known here.
    lines: Vec<MessageLine>,
}

/// What one session's chat says about who messaged it.
struct Heard {
    chat_len: usize,
    senders: Vec<Sender>,
}

/// One session that messaged the receiver, by its `agentium:` ref.
struct Sender {
    session_ref: String,
    count: usize,
    /// When its newest message was sent, in unix seconds.
    last_sent_at: Option<u64>,
    /// When, in egui seconds, the packet for its newest message set off.
    packet_since: Option<f64>,
}

/// Messages from one session to another.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MessageLine {
    pub from: SessionId,
    pub to: SessionId,
    /// How many messages `from` has sent `to`.
    pub count: usize,
    /// When the newest was sent, in unix seconds, if known.
    pub last_sent_at: Option<u64>,
    /// When, in egui seconds, a packet set off for a message that just
    /// arrived.
    pub packet_since: Option<f64>,
}

impl MessageLine {
    /// The line's opacity at unix time `wall`: full, then fading over the
    /// last [`FADE_SECS`] of [`LINE_SECS`], then 0. A line whose last
    /// message has no time is history, and isn't drawn.
    pub fn opacity(&self, wall: f64) -> f32 {
        let Some(sent) = self.last_sent_at else {
            return 0.0;
        };
        let left = LINE_SECS - (wall - sent as f64).max(0.0);
        (left / FADE_SECS).clamp(0.0, 1.0) as f32
    }

    /// How far along the line the packet is at egui time `now`, 0 to 1, or
    /// `None` when no packet is travelling.
    pub fn packet(&self, now: f64) -> Option<f32> {
        let t = (now - self.packet_since?) / PACKET_SECS;
        (0.0..1.0).contains(&t).then_some(t as f32)
    }

    /// Seconds until the line looks different at egui time `now` and unix
    /// time `wall`, given `frame`, the time a moving thing waits between
    /// frames. `None` once it has faded out and no packet travels it.
    pub fn next_change(&self, now: f64, wall: f64, frame: f64) -> Option<f64> {
        if self.packet(now).is_some() {
            return Some(frame);
        }
        let sent = self.last_sent_at? as f64;
        let age = wall - sent;
        if age >= LINE_SECS {
            return None;
        }
        let fade_starts = LINE_SECS - FADE_SECS;
        if age < fade_starts {
            return Some(fade_starts - age);
        }
        Some(FADE_FRAME_SECS)
    }
}

/// How often a fading line is redrawn: the fade is a minute long, so four
/// steps a second is smooth enough.
const FADE_FRAME_SECS: f64 = 0.25;

impl MessageLines {
    /// Bring the lines up to date with `sessions` at egui time `now` and
    /// unix time `wall`. `relaid` says the map's membership changed, so a
    /// sender that wasn't known before may be now. Cheap when no chat grew.
    pub fn sync(&mut self, sessions: &SessionManager, now: f64, wall: u64, relaid: bool) {
        let mut changed = false;
        let mut seen = 0;
        for session in sessions.iter().filter(|s| s.agentic.is_some()) {
            seen += 1;
            let len = session.chat.len();
            let prev = self.heard.get(&session.id);
            if prev.is_some_and(|h| h.chat_len == len) {
                continue;
            }
            let heard = scan(session, prev, now, wall);
            self.heard.insert(session.id, heard);
            changed = true;
        }
        if seen != self.heard.len() {
            self.heard
                .retain(|id, _| sessions.get(*id).is_some_and(|s| s.agentic.is_some()));
            changed = true;
        }
        if changed || relaid {
            self.resolve(sessions);
        }
    }

    /// The lines between sessions known here, sorted by sender then
    /// receiver.
    pub fn lines(&self) -> &[MessageLine] {
        &self.lines
    }

    /// Turn each heard sender's ref into its session, dropping a sender
    /// that isn't known here and a session messaging itself.
    fn resolve(&mut self, sessions: &SessionManager) {
        self.lines.clear();
        for (&to, heard) in &self.heard {
            for sender in &heard.senders {
                let Some(from) = sessions.find_by_ref(&sender.session_ref) else {
                    continue;
                };
                if from == to {
                    continue;
                }
                self.lines.push(MessageLine {
                    from,
                    to,
                    count: sender.count,
                    last_sent_at: sender.last_sent_at,
                    packet_since: sender.packet_since,
                });
            }
        }
        self.lines.sort_unstable_by_key(|l| (l.from, l.to));
    }
}

/// Who messaged `session`, from its chat. A sender with more messages than
/// `prev` knew of, whose newest is fresh, sends a packet from `now`; one
/// with none new keeps the packet it had. A session `prev` hadn't seen is
/// loading its history, so nothing in it is an arrival.
fn scan(session: &ChatSession, prev: Option<&Heard>, now: f64, wall: u64) -> Heard {
    let mut senders: Vec<Sender> = Vec::new();
    for msg in &session.chat {
        let Message::User(user) = msg else {
            continue;
        };
        let Some(from) = user.from_session.as_deref() else {
            continue;
        };
        let at = senders.iter().position(|s| s.session_ref == from);
        let sender = match at {
            Some(i) => &mut senders[i],
            None => {
                senders.push(Sender {
                    session_ref: from.to_owned(),
                    count: 0,
                    last_sent_at: None,
                    packet_since: None,
                });
                senders.last_mut().expect("just pushed")
            }
        };
        sender.count += 1;
        sender.last_sent_at = sender.last_sent_at.max(user.sent_at);
    }
    for sender in &mut senders {
        let Some(before) = prev else {
            continue;
        };
        let known = before
            .senders
            .iter()
            .find(|s| s.session_ref == sender.session_ref);
        let grew = known.is_none_or(|k| sender.count > k.count);
        let fresh = sender
            .last_sent_at
            .is_some_and(|t| wall.saturating_sub(t) <= FRESH_SECS);
        sender.packet_since = if grew && fresh {
            Some(now)
        } else {
            known.and_then(|k| k.packet_since)
        };
    }
    Heard {
        chat_len: session.chat.len(),
        senders,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendType;
    use crate::config::AiMode;
    use crate::messages::UserMessage;
    use std::path::PathBuf;

    const WALL: u64 = 1_800_000_000;

    /// Two agentic sessions with known refs.
    struct Pair {
        sm: SessionManager,
        a: SessionId,
        b: SessionId,
    }

    fn two_sessions() -> Pair {
        let mut sm = SessionManager::new();
        let mut add = || sm.new_session(PathBuf::from("/w"), AiMode::Agentic, BackendType::Claude);
        let a = add();
        let b = add();
        sm.rebuild_groups();
        Pair { sm, a, b }
    }

    fn session_ref(sm: &SessionManager, id: SessionId) -> String {
        let event_id = sm
            .get(id)
            .and_then(|s| s.agentic.as_ref())
            .expect("agentic")
            .event_session_id();
        agentium_core::wordid::session_ref(event_id)
    }

    fn send(sm: &mut SessionManager, from: &str, to: SessionId, sent_at: u64) {
        sm.get_mut(to)
            .expect("receiver")
            .chat
            .push(Message::User(UserMessage {
                from_session: Some(from.to_owned()),
                sent_at: Some(sent_at),
                ..UserMessage::from("hi")
            }));
    }

    #[test]
    fn a_message_arriving_draws_a_line_and_sends_a_packet() {
        let Pair { mut sm, a, b } = two_sessions();
        let a_ref = session_ref(&sm, a);
        let mut lines = MessageLines::default();
        lines.sync(&sm, 1.0, WALL, true);
        assert!(lines.lines().is_empty());

        send(&mut sm, &a_ref, b, WALL);
        lines.sync(&sm, 2.0, WALL, false);
        let line = lines.lines()[0];
        assert_eq!((line.from, line.to, line.count), (a, b, 1));
        assert_eq!(line.packet(2.0), Some(0.0));
        assert!(line.packet(2.0 + PACKET_SECS).is_none());
        assert_eq!(line.opacity(WALL as f64), 1.0);
    }

    #[test]
    fn history_draws_no_packet_and_old_lines_fade_out() {
        let Pair { mut sm, a, b } = two_sessions();
        let a_ref = session_ref(&sm, a);
        // Already in the chat when the map first looks: history.
        send(&mut sm, &a_ref, b, WALL);
        let mut lines = MessageLines::default();
        lines.sync(&sm, 1.0, WALL, true);
        let line = lines.lines()[0];
        assert_eq!(line.packet(1.0), None);

        let wall = WALL as f64;
        assert_eq!(line.opacity(wall + LINE_SECS - FADE_SECS), 1.0);
        let half = line.opacity(wall + LINE_SECS - FADE_SECS / 2.0);
        assert!((half - 0.5).abs() < 1e-6, "{half}");
        assert_eq!(line.opacity(wall + LINE_SECS), 0.0);
        assert_eq!(line.next_change(1.0, wall + LINE_SECS, 0.03), None);
        assert_eq!(
            line.next_change(1.0, wall, 0.03),
            Some(LINE_SECS - FADE_SECS)
        );
    }

    #[test]
    fn a_stale_message_turning_up_sends_no_packet() {
        let Pair { mut sm, a, b } = two_sessions();
        let a_ref = session_ref(&sm, a);
        let mut lines = MessageLines::default();
        lines.sync(&sm, 1.0, WALL, true);
        send(&mut sm, &a_ref, b, WALL - FRESH_SECS - 1);
        lines.sync(&sm, 2.0, WALL, false);
        assert_eq!(lines.lines()[0].packet(2.0), None);
    }

    #[test]
    fn unknown_senders_and_self_messages_draw_nothing() {
        let Pair { mut sm, a, b } = two_sessions();
        let b_ref = session_ref(&sm, b);
        send(&mut sm, "agentium:nobody-knows-this", b, WALL);
        send(&mut sm, &b_ref, b, WALL);
        let mut lines = MessageLines::default();
        lines.sync(&sm, 1.0, WALL, true);
        assert!(lines.lines().is_empty());

        // A deleted sender's line goes with it.
        let a_ref = session_ref(&sm, a);
        send(&mut sm, &a_ref, b, WALL);
        lines.sync(&sm, 2.0, WALL, false);
        assert_eq!(lines.lines().len(), 1);
        sm.delete_session(a);
        sm.rebuild_groups();
        lines.sync(&sm, 3.0, WALL, true);
        assert!(lines.lines().is_empty());
    }
}
