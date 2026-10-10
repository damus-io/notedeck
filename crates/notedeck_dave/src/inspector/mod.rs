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

mod background;
mod files;
mod subagents;
mod usage;

pub use background::{BackgroundParts, BackgroundRow, BackgroundState, OutputTails};
pub use files::{
    claude_temp_root, list_session_temp, project_slug, session_temp_dir, FileRow, FileRows,
    FilesParts, FilesState, GroupRows, TempFile, TempGroup, TempListing, GROUP_CAP,
};
pub use subagents::{subagent, ProgressById, SubagentRow, SubagentRows, SubagentState};
pub use usage::{Compaction, Sparkline, Tokens, TurnUsage, UsageHistory, HISTORY_CAP};

use crate::messages::{
    CliState, DaveApiResponse, RateLimitInfo, RateLimitStatus, RateLimitWindow,
    RateLimitWindowKind, SessionInfo, WaitingOn,
};
use crate::ui::task_list::TodoCounts;
use notedeck::Localization;

/// What a session's Details tab knows about it, folded from the responses its
/// backend streams. Owned by [`ChatSession`](crate::session::ChatSession).
///
/// Each inspector section adds the fields it draws from and grows [`Self::apply`]
/// to fill them.
#[derive(Debug, Default)]
pub struct SessionInspector {
    /// The CLI's latest `system/init`: model, versions, tools, MCP servers,
    /// skills, plugins. Replaced each time a CLI (re)starts.
    pub session_info: Option<SessionInfo>,
    /// Whether the Claude CLI process is running and when it will be reaped.
    /// `None` until the backend first reports it (only the Claude backend does).
    pub cli: Option<CliState>,
    /// This session's `agentium:<word-id>` ref, derived once from its
    /// kind-31988 d-tag (see [`Self::set_session_id`]).
    pub session_ref: Option<String>,
    /// Display strings the Overview builds when their inputs change.
    pub labels: OverviewLabels,
    /// Context, tokens and cost, turn by turn.
    pub usage: UsageHistory,
    /// Display strings the Usage card builds when their inputs change.
    pub usage_labels: UsageLabels,
    /// The account's rate-limit windows as this session's backend last
    /// reported them. A local session's card shows the account-wide value on
    /// `Dave` instead, which any local session may have updated since; this
    /// one is what a remote session has.
    pub rate_limit: Option<RateLimitInfo>,
    /// Display strings the Rate limits card and status-bar pill build when
    /// their inputs change.
    pub rate_limit_labels: RateLimitLabels,
    /// Background tasks, wake-ups and crons, with their display rows and
    /// output tails.
    pub background: BackgroundState,
    /// Each subagent's tokens and tool uses, and the Subagents card's rows.
    pub subagents: SubagentState,
    /// The Tasks card's "3/5" header, keyed by the task list's counts. The
    /// list itself is the chat's latest `TodoUpdate`, so it needs no state.
    pub tasks_header: LabelMemo<TodoCounts>,
    /// The session's Claude Code temp dir (scratchpad and task output), as
    /// last listed.
    pub files: FilesState,
}

impl SessionInspector {
    /// Fold one backend response into the inspector. Called from
    /// `stream_events::apply_response` before the response is consumed, so
    /// every response the session sees reaches here.
    pub fn apply(&mut self, res: &DaveApiResponse) {
        match res {
            DaveApiResponse::SessionInfo(info) => self.session_info = Some(info.clone()),
            DaveApiResponse::CliState(cli) => self.cli = Some(*cli),
            DaveApiResponse::RateLimit(info) => {
                self.rate_limit = Some(info.merged_onto(self.rate_limit.as_ref()));
            }
            DaveApiResponse::BackgroundWork(entries) => self.background.set(entries.clone()),
            DaveApiResponse::SubagentProgress(progress) => self.subagents.apply(progress),
            _ => self.usage.apply(res, crate::session::now_unix()),
        }
    }

    /// Record the session's stable id (its kind-31988 d-tag) and derive its
    /// `agentium:` ref from it. Only the first call derives anything.
    pub fn set_session_id(&mut self, event_session_id: &str) {
        if self.session_ref.is_none() {
            self.session_ref = Some(agentium_core::wordid::session_ref(event_session_id));
        }
    }
}

/// The Overview's computed lines, each rebuilt only when what it shows
/// changes.
#[derive(Debug, Default)]
pub struct OverviewLabels {
    /// What a Waiting session is parked on.
    pub waiting: LabelMemo<WaitingOn>,
    /// The CLI process line.
    pub cli: LabelMemo<CliState>,
    /// The count chips: "3 tools", "1 plugin", …
    pub tools: LabelMemo<usize>,
    pub skills: LabelMemo<usize>,
    pub plugins: LabelMemo<usize>,
    pub agents: LabelMemo<usize>,
}

/// The Usage card's computed lines, each rebuilt only when what it shows
/// changes.
#[derive(Debug, Default)]
pub struct UsageLabels {
    /// "123K / 200K · 62%", keyed by context tokens and window.
    pub context: LabelMemo<ContextFill>,
    /// The legend under the bar, one count per kind of token: cache read,
    /// cache write, input, output.
    pub breakdown: [LabelMemo<u64>; 4],
    /// Cost and turn counts, keyed by the history's generation.
    pub totals: LabelMemo<u64>,
    /// "2 compactions", keyed by the count.
    pub compactions_heading: LabelMemo<usize>,
    /// The compaction lines, keyed by the history's generation.
    pub compactions: LabelMemo<u64>,
}

/// Most windows a rate-limit report carries: five-hour, seven-day,
/// seven-day with overage, and the limiting one when it is none of those.
pub const RATE_LIMIT_WINDOWS: usize = 4;

/// The Rate limits card's computed lines and the status-bar pill's, each
/// rebuilt only when what it shows changes.
#[derive(Debug, Default)]
pub struct RateLimitLabels {
    /// "Allowed", "Near the limit", "Limited".
    pub status: LabelMemo<RateLimitStatus>,
    /// Each shown window's name, by its position in the card.
    pub names: [LabelMemo<RateLimitWindowKind>; RATE_LIMIT_WINDOWS],
    /// Each shown window's "82% · resets 3:42 PM", keyed by the window and
    /// whether its reset time has passed.
    pub windows: [LabelMemo<WindowReading>; RATE_LIMIT_WINDOWS],
    /// The status-bar pill, keyed by status and the limiting window's
    /// percentage.
    pub pill: LabelMemo<PillReading>,
}

/// How full the context is: the Usage card's context line's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextFill {
    /// Tokens in the context.
    pub context: u64,
    /// The model's context window, in tokens.
    pub window: u64,
}

/// One rate-limit window as its line in the Rate limits card shows it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowReading {
    /// Which window it is.
    pub kind: RateLimitWindowKind,
    /// How much of it is used, and when it resets.
    pub window: RateLimitWindow,
    /// Whether its reset time has passed.
    pub passed: bool,
}

/// What the status-bar rate-limit pill shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PillReading {
    /// The account's rate-limit status.
    pub status: RateLimitStatus,
    /// How full the limiting window is, in percent.
    pub percent: u32,
}

/// A localized display string built from a key the first frame it is drawn
/// with that key, and borrowed on every frame after, so a `*_ui` function can
/// show a formatted value without formatting it sixty times a second.
///
/// It also rebuilds when the locale changes, which the key doesn't cover.
/// `T` is the built value: one string by default, or a whole list of rows.
#[derive(Debug)]
pub struct LabelMemo<K, T = String> {
    /// The key and locale generation `text` was built for.
    built_for: Option<(K, u64)>,
    text: T,
}

// Not derived: an unbuilt memo needs no default key.
impl<K, T: Default> Default for LabelMemo<K, T> {
    fn default() -> Self {
        Self {
            built_for: None,
            text: T::default(),
        }
    }
}

impl<K: Copy + PartialEq, T> LabelMemo<K, T> {
    /// The value for `key`, built with `build` if the key or the locale
    /// changed since the last call.
    pub fn get(
        &mut self,
        key: K,
        i18n: &mut Localization,
        build: impl FnOnce(K, &mut Localization) -> T,
    ) -> &T {
        let built_for = (key, i18n.cache_generation());
        if self.built_for != Some(built_for) {
            self.text = build(key, i18n);
            self.built_for = Some(built_for);
        }
        &self.text
    }
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
    /// The Overview list a count chip expanded, if any.
    pub overview_list: Option<OverviewList>,
}

/// A list behind one of the Overview's count chips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverviewList {
    Tools,
    Skills,
    Plugins,
    Agents,
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

    /// Expand `list` under the Overview's chips, or collapse it if it is the
    /// one already open.
    pub fn toggle_overview_list(&mut self, list: OverviewList) {
        self.overview_list = if self.overview_list == Some(list) {
            None
        } else {
            Some(list)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::KindedWindow;

    /// The Overview's data arrives as responses: init replaces the last
    /// init, CLI state is a level.
    #[test]
    fn folds_session_info_and_cli_state() {
        let mut inspector = SessionInspector::default();
        inspector.apply(&DaveApiResponse::SessionInfo(SessionInfo {
            tools: vec!["Bash".into(), "Read".into()],
            skills: vec!["headway".into()],
            ..Default::default()
        }));
        inspector.apply(&DaveApiResponse::CliState(CliState {
            connected: true,
            reap_at: Some(42),
        }));

        let info = inspector.session_info.as_ref().unwrap();
        assert_eq!(info.tools.len(), 2);
        assert_eq!(info.skills, ["headway"]);
        assert_eq!(inspector.cli.and_then(|cli| cli.reap_at), Some(42));
    }

    /// A rate-limit report replaces the last, except that one without
    /// per-window usage (a status change alone) keeps the windows it had.
    #[test]
    fn folds_rate_limits_keeping_windows_a_bare_status_lacks() {
        let five_hour = RateLimitWindow {
            utilization: 0.82,
            resets_at: 1_791_590_400,
        };
        let mut inspector = SessionInspector::default();
        inspector.apply(&DaveApiResponse::RateLimit(RateLimitInfo {
            status: RateLimitStatus::AllowedWarning,
            five_hour: Some(five_hour),
            ..Default::default()
        }));
        inspector.apply(&DaveApiResponse::RateLimit(RateLimitInfo {
            status: RateLimitStatus::Rejected,
            limiting: Some(RateLimitWindowKind::FiveHour),
            utilization: Some(1.0),
            resets_at: Some(1_791_590_400),
            ..Default::default()
        }));

        let info = inspector.rate_limit.unwrap();
        assert_eq!(info.status, RateLimitStatus::Rejected);
        assert_eq!(info.five_hour, Some(five_hour));
        // With per-window usage, the windows are what the card shows; the
        // limiting window isn't repeated.
        assert_eq!(
            info.windows().collect::<Vec<_>>(),
            [KindedWindow {
                kind: RateLimitWindowKind::FiveHour,
                window: five_hour
            }]
        );

        // A report with windows of its own replaces them.
        let seven_day = RateLimitWindow {
            utilization: 0.1,
            resets_at: 1_792_022_400,
        };
        inspector.apply(&DaveApiResponse::RateLimit(RateLimitInfo {
            seven_day: Some(seven_day),
            ..Default::default()
        }));
        let info = inspector.rate_limit.unwrap();
        assert_eq!(info.five_hour, None);
        assert_eq!(info.seven_day, Some(seven_day));
    }

    /// Without per-window usage, the card shows the limiting window alone.
    #[test]
    fn rate_limit_windows_fall_back_to_the_limiting_one() {
        let info = RateLimitInfo {
            status: RateLimitStatus::AllowedWarning,
            limiting: Some(RateLimitWindowKind::SevenDayOpus),
            utilization: Some(0.9),
            resets_at: Some(7),
            ..Default::default()
        };
        assert_eq!(
            info.windows().collect::<Vec<_>>(),
            [KindedWindow {
                kind: RateLimitWindowKind::SevenDayOpus,
                window: RateLimitWindow {
                    utilization: 0.9,
                    resets_at: 7
                }
            }]
        );
        assert!(info.needs_attention());
        assert!(!RateLimitInfo::default().needs_attention());
    }

    /// A memo builds once per key, not once per call.
    #[test]
    fn label_memo_rebuilds_only_on_a_new_key() {
        let mut i18n = Localization::default();
        let mut memo = LabelMemo::default();
        let mut builds = 0;
        for key in [1, 1, 2, 2, 1] {
            memo.get(key, &mut i18n, |k, _| {
                builds += 1;
                k.to_string()
            });
        }
        assert_eq!(builds, 3);
        assert_eq!(memo.get(1, &mut i18n, |_, _| unreachable!()), "1");
    }

    #[test]
    fn session_ref_is_derived_once() {
        let mut inspector = SessionInspector::default();
        inspector.set_session_id("abc");
        let first = inspector.session_ref.clone();
        inspector.set_session_id("other");
        assert_eq!(inspector.session_ref, first);
        assert!(first.is_some_and(|r| r.starts_with("agentium:")));
    }
}
