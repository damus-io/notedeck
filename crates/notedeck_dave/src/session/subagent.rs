//! Claude subagents as virtual sessions.
//!
//! Each subagent row in a session's chat ([`Message::Subagent`]) gets a
//! session of its own in the [`SessionManager`], so it is viewed and switched
//! to through the normal session view: chat, tool rows, its permission
//! prompts. A virtual session never runs a backend. Its chat is rebuilt from
//! the parent's row ([`subagent_chat`]) whenever the parent's subagents move,
//! so the row stays the one source of truth: live responses fill it, and a
//! restart or a remote viewer folds it back out of the parent's
//! `role=subagent` notes, and the virtual session follows either way.
//!
//! Nothing that walks every session needs to know about them by name: they
//! have no [`AgenticSessionData`](super::AgenticSessionData), no channel and
//! never go state-dirty, so state publishing, inspector snapshots, the
//! conversation subscription and event processing all skip them, the way they
//! skip a pending placeholder.

use super::{ChatSession, SessionId, SessionManager};
use crate::agent_status::AgentStatus;
use crate::messages::{
    AssistantMessage, PermissionRequest, SubagentInfo, SubagentStatus, SubagentTranscript,
    TranscriptKind, UserMessage,
};
use crate::tools::ToolResponse;
use crate::Message;
use agentium_core::session_events::SessionRelation;

/// What makes a session a subagent's virtual session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentOf {
    /// The session whose backend runs the subagent.
    pub parent: SessionId,
    /// The subagent's launching Task/Agent tool_use id
    /// ([`SubagentInfo::task_id`]).
    pub task_id: String,
    /// The CLI's id for the subagent ([`SubagentInfo::agent_id`]), once known.
    pub agent_id: Option<String>,
    /// The status the session shows, set at each sync (see
    /// [`virtual_status`]).
    pub status: AgentStatus,
    /// Whether the subagent runs in the background
    /// ([`SubagentInfo::background`]); the list marks it ○ rather than ●.
    pub background: bool,
}

/// What a parent's virtual sessions were last built from. When the parent's
/// current stamp differs, they are rebuilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentStamp {
    /// Bumped whenever a subagent row changes or the chat is replaced.
    rev: u64,
    /// Local permission prompts still waiting, and answers recorded: either
    /// moving can change a subagent's prompt rows.
    pending_permissions: usize,
    responded_permissions: usize,
    /// The parent's own status, which decides whether a subagent that never
    /// reported an end is still running.
    parent_status: AgentStatus,
}

/// The hover text on a virtual session's disabled composer.
pub const COMPOSER_DISABLED: &str = "messaging subagents: headway:dave/large-denial-card";

/// The status a subagent's session shows.
///
/// A running subagent is working, or needs input while one of its prompts is
/// unanswered. Once its parent is no longer working or waiting on it (the
/// process ended, or the row was rebuilt from notes after a restart) nothing
/// can still be running it, so it reads as done: read-only history.
pub fn virtual_status(
    info: &SubagentInfo,
    has_pending_prompt: bool,
    parent_status: AgentStatus,
) -> AgentStatus {
    match info.status {
        SubagentStatus::Completed => AgentStatus::Done,
        SubagentStatus::Failed => AgentStatus::Error,
        SubagentStatus::Running if has_pending_prompt => AgentStatus::NeedsInput,
        SubagentStatus::Running => match parent_status {
            AgentStatus::Working | AgentStatus::NeedsInput | AgentStatus::Waiting => {
                AgentStatus::Working
            }
            _ => AgentStatus::Done,
        },
    }
}

/// A virtual session's title: the subagent's type and what it was asked.
pub fn subagent_title(info: &SubagentInfo) -> String {
    format!("{}: {}", info.subagent_type, info.description)
}

/// The chat a subagent's session shows, built from its parent's row.
///
/// What it was asked opens it as the user turn. Then its text and thinking,
/// each block in its place among its tool results (see
/// [`TranscriptRun::after_tools`](crate::messages::TranscriptRun::after_tools)),
/// then the permission prompts it raised, in the order the parent shows them.
/// A completed subagent that forwarded no text shows its result; a failed one
/// its error.
pub fn subagent_chat<'a>(
    info: &SubagentInfo,
    prompts: impl Iterator<Item = &'a PermissionRequest>,
) -> Vec<Message> {
    let mut chat = Vec::with_capacity(2 + info.transcript.runs().len() + info.tool_results.len());
    chat.push(Message::User(UserMessage::from(info.description.as_str())));
    if info.transcript.dropped() {
        chat.push(Message::System(
            SubagentTranscript::DROPPED_MARKER.to_string(),
        ));
    }

    let mut tools = info.tool_results.iter().enumerate().peekable();
    for run in info.transcript.runs() {
        while let Some((_, tool)) = tools.next_if(|(i, _)| *i < run.after_tools) {
            chat.push(tool_row(tool));
        }
        let text = match run.kind {
            TranscriptKind::Text => run.text.clone(),
            TranscriptKind::Thinking => format!("*Thinking*\n\n{}", run.text),
        };
        chat.push(Message::Assistant(AssistantMessage::from_text(text)));
    }
    chat.extend(tools.map(|(_, tool)| tool_row(tool)));

    chat.extend(
        prompts
            .filter(|req| req.parent_task_id.as_deref() == Some(info.task_id.as_str()))
            .cloned()
            .map(Message::PermissionRequest),
    );

    match info.status {
        SubagentStatus::Failed => chat.push(Message::Error(info.output.clone())),
        SubagentStatus::Completed if info.transcript.is_empty() && !info.output.is_empty() => {
            chat.push(Message::Assistant(AssistantMessage::from_text(
                info.output.clone(),
            )));
        }
        _ => {}
    }
    chat
}

fn tool_row(tool: &crate::messages::ExecutedTool) -> Message {
    Message::ToolResponse(ToolResponse::executed_tool(tool.clone()))
}

/// Whether `req` is a prompt still waiting on the user.
fn is_unanswered(req: &PermissionRequest, session: &ChatSession) -> bool {
    req.response.is_none()
        && session
            .agentic
            .as_ref()
            .is_none_or(|a| !a.permissions.responded.contains_key(&req.id))
}

/// One subagent's session as a sync builds it, before it is installed.
struct Built {
    task_id: String,
    agent_id: Option<String>,
    title: String,
    chat: Vec<Message>,
    status: AgentStatus,
    background: bool,
}

impl ChatSession {
    /// Note that a subagent row changed, so the virtual sessions rebuild.
    pub fn mark_subagents_changed(&mut self) {
        self.subagent_rev = self.subagent_rev.wrapping_add(1);
    }

    /// What this session's virtual sessions would be built from now.
    fn subagent_stamp(&self) -> SubagentStamp {
        let permissions = self.agentic.as_ref().map(|a| &a.permissions);
        SubagentStamp {
            rev: self.subagent_rev,
            pending_permissions: permissions.map_or(0, |p| p.pending.len()),
            responded_permissions: permissions.map_or(0, |p| p.responded.len()),
            parent_status: self.status(),
        }
    }

    /// Whether this session's virtual sessions are out of date.
    fn subagents_stale(&self) -> bool {
        self.subagent_of.is_none() && self.subagent_synced != Some(self.subagent_stamp())
    }

    /// Build the virtual sessions of every subagent row, in chat order.
    fn build_subagent_sessions(&self) -> Vec<Built> {
        let prompts = || {
            self.chat.iter().filter_map(|msg| match msg {
                Message::PermissionRequest(req) if req.parent_task_id.is_some() => Some(req),
                _ => None,
            })
        };
        let parent_status = self.status();
        self.chat
            .iter()
            .filter_map(|msg| match msg {
                Message::Subagent(info) => Some(info),
                _ => None,
            })
            .map(|info| {
                let pending = prompts().any(|req| {
                    req.parent_task_id.as_deref() == Some(info.task_id.as_str())
                        && is_unanswered(req, self)
                });
                Built {
                    task_id: info.task_id.clone(),
                    agent_id: info.agent_id.clone(),
                    title: subagent_title(info),
                    chat: subagent_chat(info, prompts()),
                    status: virtual_status(info, pending, parent_status),
                    background: info.background,
                }
            })
            .collect()
    }

    /// A virtual session for one of `parent`'s subagents. It inherits where
    /// and how the parent runs (host, cwd, project, model, backend), never
    /// runs a backend of its own and never publishes state.
    fn new_subagent(id: SessionId, parent: &ChatSession, built: Built) -> Self {
        let mut session = ChatSession::new_pending_placeholder(
            id,
            parent.details.cwd.clone().unwrap_or_default(),
            parent.details.hostname.clone(),
            parent.backend_type,
            String::new(),
            None,
        );
        session.pending_created_at = None;
        session.spawn_id = None;
        session.source = parent.source;
        session.details.title = built.title;
        session.details.relation = SessionRelation::Child;
        session
            .details
            .home_dir
            .clone_from(&parent.details.home_dir);
        session
            .details
            .project_slug
            .clone_from(&parent.details.project_slug);
        session
            .details
            .project_root
            .clone_from(&parent.details.project_root);
        session.details.model.clone_from(&parent.details.model);
        session.last_activity = parent.last_activity;
        session.subagent_of = Some(SubagentOf {
            parent: parent.id,
            task_id: built.task_id,
            agent_id: built.agent_id,
            status: built.status,
            background: built.background,
        });
        session.replace_chat(built.chat);
        session.update_status();
        session.state_dirty = false;
        session
    }
}

impl SessionManager {
    /// Bring every session's subagent sessions up to date with its subagent
    /// rows, creating the ones that are new. A parent whose rows haven't moved
    /// since the last sync costs one stamp comparison, so this runs every
    /// update.
    pub fn sync_subagent_sessions(&mut self) {
        while let Some(parent_id) = self.next_stale_parent() {
            self.sync_subagents_of(parent_id);
        }
    }

    fn next_stale_parent(&self) -> Option<SessionId> {
        self.order.iter().copied().find(|id| {
            self.sessions
                .get(id)
                .is_some_and(ChatSession::subagents_stale)
        })
    }

    fn sync_subagents_of(&mut self, parent_id: SessionId) {
        let Some(parent) = self.sessions.get_mut(&parent_id) else {
            return;
        };
        parent.subagent_synced = Some(parent.subagent_stamp());
        let built = parent.build_subagent_sessions();

        let mut added = false;
        for b in built {
            if let Some(id) = self.subagent_session(parent_id, &b.task_id) {
                let Some(session) = self.sessions.get_mut(&id) else {
                    continue;
                };
                session.details.title = b.title;
                session.replace_chat(b.chat);
                if let Some(link) = &mut session.subagent_of {
                    link.agent_id = b.agent_id;
                    link.status = b.status;
                    link.background = b.background;
                }
                session.update_status();
                session.state_dirty = false;
                continue;
            }
            let id = self.next_id;
            self.next_id += 1;
            let Some(parent) = self.sessions.get(&parent_id) else {
                return;
            };
            let session = ChatSession::new_subagent(id, parent, b);
            self.sessions.insert(id, session);
            self.order.push(id);
            added = true;
        }
        if added {
            self.rebuild_groups();
        }
    }

    /// The virtual session of `parent`'s subagent `task_id`, if it has one.
    pub fn subagent_session(&self, parent: SessionId, task_id: &str) -> Option<SessionId> {
        self.sessions.values().find_map(|s| {
            let link = s.subagent_of.as_ref()?;
            (link.parent == parent && link.task_id == task_id).then_some(s.id)
        })
    }

    /// The session whose backend serves `id`: its parent for a subagent's
    /// virtual session, else `id` itself. Commands a virtual session can't
    /// carry out alone (answering a prompt, stopping) go there.
    pub fn backing_id(&self, id: SessionId) -> SessionId {
        self.sessions
            .get(&id)
            .and_then(|s| s.subagent_of.as_ref())
            .map_or(id, |link| link.parent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendType;
    use crate::config::AiMode;
    use crate::messages::{ExecutedTool, PermissionResponseType};
    use std::path::PathBuf;

    fn spawned(task_id: &str, background: bool) -> Message {
        Message::Subagent(SubagentInfo {
            task_id: task_id.into(),
            description: "find the parser".into(),
            subagent_type: "Explore".into(),
            status: SubagentStatus::Running,
            output: String::new(),
            max_output_size: 4000,
            tool_results: Vec::new(),
            background,
            agent_id: None,
            transcript: Default::default(),
        })
    }

    fn tool(parent: &str, summary: &str) -> ExecutedTool {
        ExecutedTool {
            tool_name: "Read".into(),
            summary: summary.into(),
            output: None,
            parent_task_id: Some(parent.into()),
            file_update: None,
            tool_use_id: None,
        }
    }

    /// A manager and the parent session in it.
    struct Setup {
        sm: SessionManager,
        parent: SessionId,
    }

    /// A manager with one agentic parent session, mid-turn.
    fn manager() -> Setup {
        let mut sm = SessionManager::new();
        let parent = sm.new_session(
            PathBuf::from("/tmp/sub"),
            AiMode::Agentic,
            BackendType::Claude,
        );
        let session = sm.get_mut(parent).unwrap();
        session.details.hostname = "hydra".into();
        session.details.model = Some("claude-opus-4-6".into());
        session.wakeup_turn = true;
        session.update_status();
        Setup { sm, parent }
    }

    fn child<'a>(sm: &'a SessionManager, parent: SessionId, task_id: &str) -> &'a ChatSession {
        let id = sm
            .subagent_session(parent, task_id)
            .expect("a virtual session");
        sm.get(id).unwrap()
    }

    /// A foreground spawn and a background one (from `task_started`) each
    /// get a session that inherits the parent's host and model, and is
    /// listed right under it.
    #[test]
    fn a_spawned_subagent_gets_a_session() {
        let Setup { mut sm, parent } = manager();
        let session = sm.get_mut(parent).unwrap();
        session.insert_turn_content(spawned("toolu_fg", false));
        session.insert_turn_content(spawned("toolu_bg", true));
        sm.sync_subagent_sessions();

        for task_id in ["toolu_fg", "toolu_bg"] {
            let sub = child(&sm, parent, task_id);
            assert_eq!(sub.details.display_title(), "Explore: find the parser");
            assert_eq!(sub.details.hostname, "hydra");
            assert_eq!(sub.details.model.as_deref(), Some("claude-opus-4-6"));
            assert_eq!(sub.status(), AgentStatus::Working);
            assert!(sub.agentic.is_none(), "no backend of its own");
            assert!(!sub.state_dirty, "never publishes state");
        }
        assert_eq!(
            sm.get_active().unwrap().id,
            parent,
            "a spawn doesn't steal focus"
        );

        let fg = sm.subagent_session(parent, "toolu_fg").unwrap();
        let bg = sm.subagent_session(parent, "toolu_bg").unwrap();
        let ids = &sm.host_groups()[0].project_groups[0].cwd_groups[0].session_ids;
        assert_eq!(
            ids,
            &vec![parent, fg, bg],
            "nested under the parent, oldest first"
        );
    }

    /// The subagent's text and tool rows show in its session, in the order
    /// they happened, and not in the parent's chat, which keeps one row.
    #[test]
    fn text_and_tools_land_in_the_subagents_session() {
        let Setup { mut sm, parent } = manager();
        let session = sm.get_mut(parent).unwrap();
        session.insert_turn_content(spawned("toolu_a", false));
        session.push_subagent_text("toolu_a", TranscriptKind::Text, "reading first");
        assert!(session.fold_tool_result(tool("toolu_a", "a.rs")).is_none());
        session.push_subagent_text("toolu_a", TranscriptKind::Text, "now the second");
        assert!(session.fold_tool_result(tool("toolu_a", "b.rs")).is_none());
        sm.sync_subagent_sessions();

        let parent_rows = sm.get(parent).unwrap().chat.len();
        assert_eq!(parent_rows, 1, "the parent keeps its one subagent row");

        let rows: Vec<String> = child(&sm, parent, "toolu_a")
            .chat
            .iter()
            .map(|m| match m {
                Message::User(u) => format!("user {}", u.as_str()),
                Message::Assistant(a) => format!("text {}", a.text()),
                Message::ToolResponse(t) => match t.responses() {
                    crate::tools::ToolResponses::ExecutedTool(e) => format!("tool {}", e.summary),
                    _ => "other".into(),
                },
                _ => "other".into(),
            })
            .collect();
        assert_eq!(
            rows,
            [
                "user find the parser",
                "text reading first",
                "tool a.rs",
                "text now the second",
                "tool b.rs",
            ]
        );

        // A later block reaches the session at the next sync.
        let session = sm.get_mut(parent).unwrap();
        session.push_subagent_text("toolu_a", TranscriptKind::Text, "done");
        sm.sync_subagent_sessions();
        assert_eq!(child(&sm, parent, "toolu_a").chat.len(), 6);
    }

    /// A prompt for the subagent's tool shows in its session, which then
    /// needs input; answering it in the parent's tracker clears that.
    #[test]
    fn a_subagents_prompt_shows_in_its_session() {
        let Setup { mut sm, parent } = manager();
        let session = sm.get_mut(parent).unwrap();
        session.insert_turn_content(spawned("toolu_a", false));
        let mut req = PermissionRequest::pending(
            uuid::Uuid::new_v4(),
            "Bash".into(),
            serde_json::json!({ "command": "rm -rf build" }),
        );
        req.parent_task_id = Some("toolu_a".into());
        let req_id = req.id;
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let agentic = session.agentic.as_mut().unwrap();
        agentic.permissions.pending.insert(req_id, tx);
        session.insert_turn_content(Message::PermissionRequest(req));
        sm.sync_subagent_sessions();

        let sub = child(&sm, parent, "toolu_a");
        assert_eq!(sub.status(), AgentStatus::NeedsInput);
        assert!(sub
            .chat
            .iter()
            .any(|m| matches!(m, Message::PermissionRequest(r) if r.id == req_id)));
        let sub_id = sub.id;
        assert_eq!(sm.backing_id(sub_id), parent, "answered through the parent");

        let session = sm.get_mut(parent).unwrap();
        let agentic = session.agentic.as_mut().unwrap();
        agentic.permissions.resolve(
            &mut session.chat,
            req_id,
            PermissionResponseType::Allowed,
            None,
            false,
            None,
        );
        session.update_status();
        sm.sync_subagent_sessions();
        assert_eq!(child(&sm, parent, "toolu_a").status(), AgentStatus::Working);
    }

    /// After a restart the parent's chat is rebuilt from its notes: the
    /// subagent comes back as a session, and one whose end was never
    /// reported reads as done, since nothing is running it any more.
    #[test]
    fn rebuilt_from_notes_after_a_restart() {
        let mut sm = SessionManager::new();
        let parent = sm.new_session(
            PathBuf::from("/tmp/sub"),
            AiMode::Agentic,
            BackendType::Claude,
        );
        let Message::Subagent(mut done) = spawned("toolu_done", false) else {
            unreachable!()
        };
        done.status = SubagentStatus::Completed;
        done.output = "the parser is in claude.rs".into();
        let session = sm.get_mut(parent).unwrap();
        session.replace_chat(vec![
            Message::User("go".into()),
            // Foreground: a running background row keeps the parent itself
            // Working (`has_running_background_subagent`).
            spawned("toolu_orphan", false),
            Message::Subagent(done),
        ]);
        session.update_status();
        sm.sync_subagent_sessions();

        assert_eq!(
            child(&sm, parent, "toolu_orphan").status(),
            AgentStatus::Done
        );
        let done = child(&sm, parent, "toolu_done");
        assert_eq!(done.status(), AgentStatus::Done);
        assert!(
            matches!(done.chat.last(), Some(Message::Assistant(a)) if a.text() == "the parser is in claude.rs"),
            "with no forwarded text, the result shows"
        );
    }

    /// Deleting the parent takes its subagents' sessions with it.
    #[test]
    fn subagent_sessions_go_with_their_parent() {
        let Setup { mut sm, parent } = manager();
        sm.get_mut(parent)
            .unwrap()
            .insert_turn_content(spawned("toolu_a", false));
        sm.sync_subagent_sessions();
        let sub = sm.subagent_session(parent, "toolu_a").unwrap();
        sm.switch_to(sub);

        assert!(sm.delete_session(parent));
        assert!(sm.get(sub).is_none());
        assert!(sm.is_empty());
        assert_eq!(sm.active_id(), None);
    }
}
