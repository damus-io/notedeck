//! The card the scene map shows beside a hovered session's node: its code
//! and full title, where it runs, its status and age, its worktree's git
//! +/-, the messages it sent and received and its subagents.
//!
//! Its text is built when the hovered session or anything the card says
//! about it changes, and kept on the [`AgentScene`](super::AgentScene)
//! between frames, so hovering a node doesn't format strings every frame.

use std::hash::{Hash, Hasher};

use egui::{Color32, RichText};
use headway::title::TitleParts;

use super::dave::FROM_SESSION_VIOLET;
use super::scene::{node_code, shown_title};
use super::session_list::{age_text, git_lines, GitLines, GIT_ADDED_COLOR, GIT_DELETED_COLOR};
use crate::agent_status::AgentStatus;
use crate::scene_edges::MessageLine;
use crate::session::{now_unix, ChatSession, SessionId, SessionManager};

/// A hovered session's card text, as last built.
pub struct HoverCard {
    /// A hash of everything the card says (see [`HoverCard::key`]).
    key: u64,
    code: String,
    headline: String,
    detail: Option<String>,
    /// `host / project / worktree`.
    path: String,
    status: AgentStatus,
    /// Time since the session's last activity, empty when there was none.
    age: String,
    git: Option<GitLines>,
    /// `2 received from QA2 · 1 sent`, empty when it has none.
    messages: String,
    subagents: Vec<SubagentLine>,
}

/// One subagent of the hovered session, listed under its card.
struct SubagentLine {
    title: String,
    status: AgentStatus,
}

/// Width of the hover card.
const CARD_WIDTH: f32 = 260.0;
/// Gap between a node and its card.
const CARD_GAP: f32 = 10.0;

impl HoverCard {
    /// The card for `session`, from `slot` when what it says hasn't changed
    /// since it was built, else built again into `slot`.
    pub fn cached<'s>(
        slot: &'s mut Option<HoverCard>,
        sessions: &SessionManager,
        messages: &[MessageLine],
        session: &ChatSession,
    ) -> &'s HoverCard {
        let key = Self::key(sessions, messages, session, now_unix());
        if slot.as_ref().is_none_or(|card| card.key != key) {
            *slot = Some(Self::build(key, sessions, messages, session));
        }
        slot.as_ref().expect("hover card was just built")
    }

    /// A hash of what the card for `session` says at `now`, computed without
    /// allocating. The age goes in by the minute, so a card shown for a
    /// while is rebuilt at most once a minute.
    fn key(
        sessions: &SessionManager,
        messages: &[MessageLine],
        session: &ChatSession,
        now: u64,
    ) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        session.id.hash(&mut h);
        shown_title(session, &sessions.card_titles).hash(&mut h);
        session.workspace().hash(&mut h);
        session.project_slug().hash(&mut h);
        session.status().hash(&mut h);
        session
            .last_activity
            .map(|t| now.saturating_sub(t) / 60)
            .hash(&mut h);
        git_lines(session).hash(&mut h);
        for line in messages
            .iter()
            .filter(|l| l.from == session.id || l.to == session.id)
        {
            line.from.hash(&mut h);
            line.to.hash(&mut h);
            line.count.hash(&mut h);
        }
        for sub in subagents_of(sessions, session.id) {
            sub.id.hash(&mut h);
            sub.status().hash(&mut h);
            sub.details.display_title().hash(&mut h);
        }
        h.finish()
    }

    fn build(
        key: u64,
        sessions: &SessionManager,
        messages: &[MessageLine],
        session: &ChatSession,
    ) -> Self {
        let title = shown_title(session, &sessions.card_titles);
        let parts = TitleParts::split(title);
        let ws = session.workspace();
        let host = if ws.host.is_empty() { "local" } else { ws.host };
        let mut path = format!("{host} / {}", session.project_slug());
        if ws.cwd != ws.project_root {
            if let Some(dir) = ws.cwd.file_name() {
                path.push_str(" / ");
                path.push_str(&dir.to_string_lossy());
            }
        }
        let mut age = String::new();
        if let Some(t) = session.last_activity {
            age_text(now_unix().saturating_sub(t), &mut age);
        }
        HoverCard {
            key,
            code: node_code(title).to_owned(),
            headline: parts.headline.to_owned(),
            detail: parts.detail.map(str::to_owned),
            path,
            status: session.status(),
            age,
            git: git_lines(session),
            messages: messages_text(sessions, messages, session.id),
            subagents: subagents_of(sessions, session.id)
                .map(|sub| SubagentLine {
                    title: sub.details.display_title().to_owned(),
                    status: sub.status(),
                })
                .collect(),
        }
    }

    /// Show the card beside the node `node` responded for, to its right
    /// unless there's no room.
    pub fn show(&self, node: &egui::Response) {
        egui::Popup::from_response(node)
            .kind(egui::PopupKind::Tooltip)
            .open(true)
            .align(egui::RectAlign::RIGHT_START)
            .align_alternatives(&[egui::RectAlign::LEFT_START, egui::RectAlign::BOTTOM_START])
            .gap(CARD_GAP)
            .width(CARD_WIDTH)
            .show(|ui| self.ui(ui));
    }

    fn ui(&self, ui: &mut egui::Ui) {
        let weak = ui.visuals().weak_text_color();
        let status_color = self.status.color();
        ui.spacing_mut().item_spacing.y = 3.0;

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(
                RichText::new(&self.code)
                    .monospace()
                    .size(11.0)
                    .color(status_color),
            );
            ui.label(RichText::new(&self.headline).size(13.0).strong());
        });
        if let Some(detail) = &self.detail {
            ui.label(RichText::new(detail).size(11.0).color(weak));
        }
        ui.label(RichText::new(&self.path).monospace().size(10.0).color(weak));
        ui.add_space(4.0);

        row(
            ui,
            |ui| {
                ui.label(
                    RichText::new(self.status.label())
                        .size(11.0)
                        .color(status_color),
                );
            },
            |ui| {
                ui.label(RichText::new(&self.age).size(11.0).color(weak));
            },
        );
        if let Some(git) = self.git {
            row(
                ui,
                |ui| {
                    ui.label(RichText::new("git").size(11.0).color(weak));
                },
                |ui| {
                    // Right to left: deleted first.
                    if git.deleted > 0 {
                        ui.label(lines_text('-', git.deleted, GIT_DELETED_COLOR));
                    }
                    if git.added > 0 {
                        ui.label(lines_text('+', git.added, GIT_ADDED_COLOR));
                    }
                },
            );
        }

        if !self.messages.is_empty() {
            row(
                ui,
                |ui| {
                    ui.label(RichText::new("messages").size(11.0).color(weak));
                },
                |ui| {
                    ui.label(
                        RichText::new(&self.messages)
                            .size(11.0)
                            .color(FROM_SESSION_VIOLET),
                    );
                },
            );
        }

        if self.subagents.is_empty() {
            return;
        }
        ui.separator();
        for sub in &self.subagents {
            row(
                ui,
                |ui| {
                    status_dot(ui, sub.status.color());
                    ui.label(RichText::new(&sub.title).size(11.0));
                },
                |ui| {
                    ui.label(RichText::new(sub.status.label()).size(11.0).color(weak));
                },
            );
        }
    }
}

/// One line of the card with `left` at its start and `right` at its end.
fn row(ui: &mut egui::Ui, left: impl FnOnce(&mut egui::Ui), right: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        left(ui);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), right);
    });
}

/// A small dot in `color`, before a subagent's title.
fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(8.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.0, color);
}

/// `+12` or `-3` in `color`.
fn lines_text(sign: char, lines: usize, color: Color32) -> RichText {
    RichText::new(format!("{sign}{lines}"))
        .size(11.0)
        .color(color)
}

/// The subagents running under session `parent`.
fn subagents_of(
    sessions: &SessionManager,
    parent: SessionId,
) -> impl Iterator<Item = &ChatSession> {
    sessions.iter().filter(move |s| {
        s.subagent_of
            .as_ref()
            .is_some_and(|link| link.parent == parent)
    })
}

/// What session `id` has sent and received, as the card says it: each
/// direction's count, naming the other session's code when there is only
/// one. `2 received from QA2 · 1 sent`, or empty for none.
fn messages_text(sessions: &SessionManager, lines: &[MessageLine], id: SessionId) -> String {
    let mut text = String::new();
    let received = Direction {
        verb: "received",
        preposition: "from",
    };
    received.push(
        &mut text,
        sessions,
        lines.iter().filter(|l| l.to == id).map(|l| Peer {
            id: l.from,
            count: l.count,
        }),
    );
    let sent = Direction {
        verb: "sent",
        preposition: "to",
    };
    sent.push(
        &mut text,
        sessions,
        lines.iter().filter(|l| l.from == id).map(|l| Peer {
            id: l.to,
            count: l.count,
        }),
    );
    text
}

/// The session on the other end of a message line, and how many messages
/// went along it.
struct Peer {
    id: SessionId,
    count: usize,
}

/// One direction of a session's messages, as [`messages_text`] words it.
struct Direction {
    verb: &'static str,
    preposition: &'static str,
}

impl Direction {
    /// Append `3 sent`, or `1 sent to QA2` when there is one peer, to
    /// `text`, after a separator if it isn't empty. Nothing for no peers.
    fn push(
        &self,
        text: &mut String,
        sessions: &SessionManager,
        mut peers: impl Iterator<Item = Peer>,
    ) {
        use std::fmt::Write;
        let Some(first) = peers.next() else {
            return;
        };
        let mut count = first.count;
        let mut only = Some(first.id);
        for peer in peers {
            count += peer.count;
            only = None;
        }
        if !text.is_empty() {
            text.push_str(" · ");
        }
        let _ = write!(text, "{count} {}", self.verb);
        let code = only
            .and_then(|peer| sessions.get(peer))
            .map(|s| node_code(shown_title(s, &sessions.card_titles)));
        if let Some(code) = code {
            let _ = write!(text, " {} {code}", self.preposition);
        }
    }
}
