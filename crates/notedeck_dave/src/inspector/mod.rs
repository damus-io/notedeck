//! Per-session inspector state: what the Details tab shows about a session.
//!
//! A Claude Code session carries machinery Dave otherwise tracks silently or
//! drops — usage, rate limits, background work, subagents, tasks, files,
//! permission rules, outputs. [`SessionInspector`] collects it, one section at
//! a time, from the responses the backend streams
//! ([`SessionInspector::apply`]), and `ui::inspector` draws it.
//!
//! [`InspectorView`] is the per-session UI state beside it: which tab the
//! session view shows and which section the Details rail points at. It is never
//! published.

use crate::messages::DaveApiResponse;

/// What a session's Details tab knows about it, folded from the responses its
/// backend streams. Owned by [`ChatSession`](crate::session::ChatSession).
///
/// Each inspector section adds the fields it draws from and grows [`Self::apply`]
/// to fill them.
#[derive(Debug, Default)]
pub struct SessionInspector {}

impl SessionInspector {
    /// Fold one backend response into the inspector. Called from
    /// `stream_events::apply_response` before the response is consumed, so
    /// every response the session sees reaches here.
    pub fn apply(&mut self, _res: &DaveApiResponse) {}
}

/// Which view the session area shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionTab {
    /// The transcript and the input box.
    #[default]
    Chat,
    /// The inspector's section cards.
    Details,
}

/// One section of the Details tab, in rail order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum InspectorSection {
    /// Model, versions, MCP servers, skills and plugins from `system/init`.
    #[default]
    Overview,
    /// Context use, cost, per-turn history and compactions.
    Usage,
    /// The five-hour and seven-day rate-limit windows.
    RateLimits,
    /// Background tasks, monitors, wake-ups and crons.
    Background,
    /// Every subagent with its status and cost.
    Subagents,
    /// The harness task list.
    Tasks,
    /// The scratchpad, task output and the files edited this session.
    Files,
    /// The permission mode, answered prompts and always-allow rules.
    Permissions,
    /// Commits, cards and pages the session produced.
    Outputs,
}

impl InspectorSection {
    /// Every section, in the order the rail and the grid show them.
    pub const ALL: [InspectorSection; 9] = [
        InspectorSection::Overview,
        InspectorSection::Usage,
        InspectorSection::RateLimits,
        InspectorSection::Background,
        InspectorSection::Subagents,
        InspectorSection::Tasks,
        InspectorSection::Files,
        InspectorSection::Permissions,
        InspectorSection::Outputs,
    ];
}

/// Which tab a session shows, and where its Details rail points. Per session,
/// so switching sessions keeps each one's tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InspectorView {
    pub tab: SessionTab,
    /// The section the rail marks.
    pub section: InspectorSection,
    /// [`Self::open`] asked the grid to bring `section` into view; the grid
    /// does so once, on the next frame it draws.
    scroll_pending: bool,
}

impl InspectorView {
    /// Show the Details tab, scrolled to `section`.
    pub fn open(&mut self, section: InspectorSection) {
        self.tab = SessionTab::Details;
        self.section = section;
        self.scroll_pending = true;
    }

    /// Show the Details tab at the section it last pointed at.
    pub fn show_details(&mut self) {
        self.tab = SessionTab::Details;
    }

    /// The section the grid should bring into view this frame, if
    /// [`Self::open`] asked for one. Reading it clears it.
    pub fn take_scroll(&mut self) -> Option<InspectorSection> {
        std::mem::take(&mut self.scroll_pending).then_some(self.section)
    }

    /// Back to the chat.
    pub fn show_chat(&mut self) {
        self.tab = SessionTab::Chat;
    }
}
