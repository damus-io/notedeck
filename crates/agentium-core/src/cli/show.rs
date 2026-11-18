//! `nd agentium show` — git-show-style detail for one resolved session.

use nostrdb::{Ndb, Transaction};
use nostrdb_net::Pubkey;

use super::list::SessionJson;
use super::term::{
    abbreviate_home, col, paint, relative_time, status_style, StatusStyle, SGR_BOLD,
    SGR_NEEDS_INPUT,
};
use crate::config::RunConfig;
use crate::messages::{Message, SubagentStatus, UsageInfo};
use crate::session_loader::{
    load_run_configs_from_ndb, load_session_messages_for_author, pending_permission_requests,
    LoadedSession, PendingPermission, SessionState,
};
use crate::session_reconstructor::latest_session_usage;

/// Everything `nd agentium show` says about one session, read in one
/// transaction and owned, so it renders ([`render`](Self::render)) and
/// serializes ([`json`](Self::json)) after the transaction is gone.
///
/// The detail: the session's `agentium:` URI + status, every kind-31988 state
/// field, the run-configs registered on its host+cwd, its latest usage snapshot
/// (from the kind-1989 archive, when present), a conversation summary (message
/// count plus every pending permission request, with the id `approve`/`deny`
/// `--request` takes), and a subagent rollup.
///
/// The subagent rollup folds the published `role=subagent` kind-1988 notes (one
/// `Message::Subagent` per task, latest status). Sessions recorded before the
/// host published those notes show no subagents; tool-result notes for the
/// `Task`/`Agent` tool are deliberately not used as a stand-in, since they can't
/// say whether a subagent is running, failed, or backgrounded.
pub struct SessionDetail {
    /// The session's kind-31988 state.
    pub state: SessionState,
    /// The run-configs on its host+cwd.
    pub run_configs: Vec<RunConfig>,
    /// Its latest usage snapshot, when the archive holds a completed turn.
    pub usage: Option<UsageInfo>,
    /// Its conversation, folded.
    summary: ConversationSummary,
}

impl SessionDetail {
    /// Resolve `selector` across the live *and* tombstoned sets (so a durable
    /// `agentium:` ref still describes a soft-deleted session) and read its
    /// detail, all through the caller's `txn`.
    pub fn load(
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
        selector: Option<&str>,
    ) -> Result<Self, String> {
        let state = super::resolve_selected(ndb, txn, author, selector)?;

        // Run-configs are keyed by (hostname, cwd); the session's own host+cwd
        // pick the configs that would run *in it* — not this machine's.
        let run_configs = matching_run_configs(ndb, txn, author, &state);

        // Usage rides the lossless kind-1989 archive; the conversation summary
        // folds the kind-1988 message stream. Both read through the caller's
        // `txn`: a second read transaction on this thread is refused by
        // nostrdb (one reader slot per thread), which would silently yield an
        // empty conversation.
        let usage = latest_session_usage(ndb, txn, &state.claude_session_id);
        let loaded = load_session_messages_for_author(ndb, txn, author, &state.claude_session_id);
        let summary = ConversationSummary::from_loaded(&loaded);

        Ok(SessionDetail {
            state,
            run_configs,
            usage,
            summary,
        })
    }

    /// The `show --json` object.
    pub fn json(&self) -> SessionDetailJson<'_> {
        SessionDetailJson {
            session: SessionJson::new(&self.state),
            run_configs: &self.run_configs,
            usage: self.usage.as_ref().map(UsageJson::from),
            conversation: ConversationJson::from(&self.summary),
            subagents: SubagentsJson::from(&self.summary.subagents[..]),
        }
    }

    /// The `show` text, as of `now` (Unix seconds); `color` paints it.
    pub fn render(&self, now: u64, color: bool) -> String {
        render_detail(
            &self.state,
            &self.run_configs,
            self.usage.as_ref(),
            &self.summary,
            now,
            color,
        )
    }
}

/// The run-configs registered for a session's host+cwd — the ones that would
/// run *inside* it. [`load_run_configs_from_ndb`] buckets configs by cwd for a
/// given hostname, so we load for the session's host and take its cwd's bucket
/// (empty when none are configured there).
///
/// [`load_run_configs_from_ndb`]: crate::session_loader::load_run_configs_from_ndb
fn matching_run_configs(
    ndb: &nostrdb::Ndb,
    txn: &Transaction,
    author: &Pubkey,
    state: &SessionState,
) -> Vec<RunConfig> {
    let mut by_cwd = load_run_configs_from_ndb(ndb, txn, author, &state.hostname);
    by_cwd
        .remove(&std::path::PathBuf::from(&state.cwd))
        .unwrap_or_default()
}

/// A folded read of a session's kind-1988 conversation for the detail view: how
/// many messages it holds, every still-unanswered permission request, and the
/// subagents it spawned. Owned (not borrowing the loaded session) so it can be
/// rendered and serialized after the transaction is dropped.
struct ConversationSummary {
    message_count: usize,
    /// The unanswered permission requests, oldest first — what `approve`/`deny`
    /// act on.
    pending: Vec<PendingPermission>,
    /// Every subagent, in spawn order, at its latest status.
    subagents: Vec<SubagentSummary>,
}

impl ConversationSummary {
    /// Fold a loaded conversation into a summary.
    fn from_loaded(loaded: &LoadedSession) -> Self {
        ConversationSummary {
            message_count: loaded.messages.len(),
            pending: pending_permission_requests(loaded),
            subagents: loaded
                .messages
                .iter()
                .filter_map(|m| match m {
                    Message::Subagent(info) => Some(SubagentSummary::from(info)),
                    _ => None,
                })
                .collect(),
        }
    }
}

/// One subagent row of the rollup: the identity and lifecycle fields of a
/// folded [`SubagentInfo`], without its (possibly large) output text.
///
/// [`SubagentInfo`]: crate::messages::SubagentInfo
struct SubagentSummary {
    task_id: String,
    subagent_type: String,
    description: String,
    status: SubagentStatus,
    background: bool,
}

impl From<&crate::messages::SubagentInfo> for SubagentSummary {
    fn from(info: &crate::messages::SubagentInfo) -> Self {
        SubagentSummary {
            task_id: info.task_id.clone(),
            subagent_type: info.subagent_type.clone(),
            description: info.description.clone(),
            status: info.status,
            background: info.background,
        }
    }
}

/// Per-status tallies over a subagent rollup. `background` counts across every
/// status, so it overlaps the other three rather than adding to them.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
struct SubagentCounts {
    running: usize,
    completed: usize,
    failed: usize,
    background: usize,
}

impl SubagentCounts {
    /// Tally `subagents` by status (and background flag).
    fn tally(subagents: &[SubagentSummary]) -> Self {
        let mut c = SubagentCounts::default();
        for s in subagents {
            match s.status {
                SubagentStatus::Running => c.running += 1,
                SubagentStatus::Completed => c.completed += 1,
                SubagentStatus::Failed => c.failed += 1,
            }
            if s.background {
                c.background += 1;
            }
        }
        c
    }
}

/// The `show --json` object: the session state (with its URI), the run-configs
/// on its host+cwd, its latest usage (absent when the archive holds no
/// completed turn), and a conversation summary. Mirrors the fields the plain
/// text view renders, structured for machine consumers.
#[derive(serde::Serialize)]
pub struct SessionDetailJson<'a> {
    session: SessionJson<'a>,
    /// `RunConfig` serializes its id/name/command (its `updated_at` is
    /// `#[serde(skip)]`), so the slice needs no wrapper.
    run_configs: &'a [RunConfig],
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<UsageJson>,
    conversation: ConversationJson,
    subagents: SubagentsJson,
}

/// The `--json` shape of a [`UsageInfo`] snapshot. `UsageInfo` isn't itself
/// `Serialize`, and we add the derived `context_tokens` (the figure the desktop
/// context bar shows) so consumers don't have to re-sum the buckets.
///
/// [`UsageInfo`]: crate::messages::UsageInfo
#[derive(serde::Serialize)]
struct UsageJson {
    input_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
    output_tokens: u64,
    context_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
    num_turns: u32,
}

impl From<&crate::messages::UsageInfo> for UsageJson {
    fn from(u: &crate::messages::UsageInfo) -> Self {
        UsageJson {
            input_tokens: u.input_tokens,
            cache_creation_input_tokens: u.cache_creation_input_tokens,
            cache_read_input_tokens: u.cache_read_input_tokens,
            output_tokens: u.output_tokens,
            context_tokens: u.context_tokens(),
            cost_usd: u.cost_usd,
            num_turns: u.num_turns,
        }
    }
}

/// The `--json` shape of a [`ConversationSummary`].
#[derive(serde::Serialize)]
struct ConversationJson {
    message_count: usize,
    pending_permissions: Vec<PendingPermissionJson>,
}

/// The `--json` shape of one [`PendingPermission`]. `perm_id` is the full id,
/// which `approve`/`deny --request` accept as a (maximal) prefix.
#[derive(serde::Serialize)]
struct PendingPermissionJson {
    perm_id: String,
    tool_name: String,
    /// Unix seconds, like every other `created_at` in the CLI's output.
    created_at: u64,
    is_question: bool,
}

impl From<&ConversationSummary> for ConversationJson {
    fn from(s: &ConversationSummary) -> Self {
        ConversationJson {
            message_count: s.message_count,
            pending_permissions: s
                .pending
                .iter()
                .map(|p| PendingPermissionJson {
                    perm_id: p.perm_id.to_string(),
                    tool_name: p.tool_name.clone(),
                    created_at: p.created_ms / 1000,
                    is_question: p.is_question,
                })
                .collect(),
        }
    }
}

/// The `--json` shape of the subagent rollup: the per-status counts flattened
/// alongside every subagent row. Present (with zero counts and no items) even
/// when the session spawned none, so consumers needn't special-case absence.
#[derive(serde::Serialize)]
struct SubagentsJson {
    #[serde(flatten)]
    counts: SubagentCounts,
    items: Vec<SubagentJson>,
}

/// The `--json` shape of one [`SubagentSummary`].
#[derive(serde::Serialize)]
struct SubagentJson {
    task_id: String,
    subagent_type: String,
    description: String,
    /// `running` / `completed` / `failed` — the wire tag value.
    status: &'static str,
    background: bool,
}

impl From<&[SubagentSummary]> for SubagentsJson {
    fn from(subagents: &[SubagentSummary]) -> Self {
        SubagentsJson {
            counts: SubagentCounts::tally(subagents),
            items: subagents
                .iter()
                .map(|s| SubagentJson {
                    task_id: s.task_id.clone(),
                    subagent_type: s.subagent_type.clone(),
                    description: s.description.clone(),
                    status: s.status.as_wire(),
                    background: s.background,
                })
                .collect(),
        }
    }
}

/// A perm-id's first 8 hex digits — enough to pick it out with `--request`.
pub fn short_perm_id(perm_id: &uuid::Uuid) -> String {
    perm_id.to_string()[..8].to_string()
}

/// Width of the label column in the detail view, sized to the longest label
/// (`cli session`, `run configs`), so values align in a second column.
const DETAIL_LABEL_W: usize = 11;

/// Render one `field: value` line of the detail body, indented under its
/// section and padded to [`DETAIL_LABEL_W`] so values line up.
fn field(label: &str, value: &str) -> String {
    format!("  {label:<DETAIL_LABEL_W$}  {value}\n")
}

/// Render the full `nd agentium show` detail block for a session as plain text.
///
/// A header line (`agentium:` URI + colored status) and the display title, then
/// the kind-31988 state fields, the host+cwd run-configs, the usage snapshot
/// (omitted entirely when `None`), the conversation summary, and the subagent
/// rollup (omitted entirely when the session spawned none). Returns an
/// owned `String` (rather than printing) so the layout is unit-testable; ANSI
/// color is applied only when `color` (stdout is a tty).
fn render_detail(
    s: &SessionState,
    run_configs: &[RunConfig],
    usage: Option<&UsageInfo>,
    summary: &ConversationSummary,
    now: u64,
    color: bool,
) -> String {
    let StatusStyle { glyph, label, sgr } = status_style(&s.status);
    let mut out = String::new();

    // Header: the sayable ref + status, then the human title.
    out.push_str(&format!(
        "{}  {}\n",
        paint(color, "90", &s.agentium_uri()),
        paint(color, sgr, &format!("{glyph} {label}")),
    ));
    let title = match s.display_title() {
        "" => "(untitled)",
        t => t,
    };
    out.push_str(&format!("{}\n\n", paint(color, SGR_BOLD, title)));

    // kind-31988 state fields. A dash stands in for an absent optional tag.
    let dash = |v: Option<&str>| v.filter(|t| !t.is_empty()).unwrap_or("-").to_string();
    out.push_str(&field("session", &s.claude_session_id));
    out.push_str(&field("cli session", &dash(s.cli_session_id.as_deref())));
    out.push_str(&field("spawn id", &dash(s.spawn_id.as_deref())));
    if let Some(issue) = s.issue_url.as_deref().filter(|i| !i.is_empty()) {
        out.push_str(&field("issue", issue));
    }
    // Lineage, only when recorded: who spawned this session, and how it relates
    // to them (a session spawned from a shell has neither).
    if let Some(parent) = s.parent_session.as_deref().filter(|p| !p.is_empty()) {
        out.push_str(&field("parent", parent));
    }
    if !s.relation.is_none() {
        out.push_str(&field("relation", s.relation.as_str()));
    }
    out.push_str(&field(
        "host",
        if s.hostname.is_empty() {
            "(unknown host)"
        } else {
            &s.hostname
        },
    ));
    out.push_str(&field("cwd", &abbreviate_home(&s.cwd, &s.home_dir)));
    out.push_str(&field("home", &dash(Some(s.home_dir.as_str()))));
    out.push_str(&field("backend", &dash(s.backend.as_deref())));
    if let Some(profile) = s.profile.as_deref().filter(|p| !p.is_empty()) {
        out.push_str(&field("profile", profile));
    }
    if let Some(spec) = s.agent_spec.as_deref().filter(|p| !p.is_empty()) {
        out.push_str(&field("agent spec", spec));
    }
    out.push_str(&field("perm mode", &dash(s.permission_mode.as_deref())));
    if let Some(ind) = s.indicator.as_deref().filter(|i| !i.is_empty()) {
        out.push_str(&field("indicator", ind));
    }
    out.push_str(&field(
        "created",
        &format!("{} ({})", relative_time(now, s.created_at), s.created_at),
    ));

    // Run-configs on the session's host+cwd.
    out.push('\n');
    out.push_str(&paint(color, SGR_BOLD, "run configs (host+cwd)"));
    out.push('\n');
    if run_configs.is_empty() {
        out.push_str("  none\n");
    } else {
        for rc in run_configs {
            out.push_str(&format!("  {}  {}\n", col(&rc.name, 16), rc.command));
        }
    }

    // Usage snapshot — only when the archive held a completed turn.
    if let Some(u) = usage {
        out.push('\n');
        out.push_str(&paint(color, SGR_BOLD, "usage"));
        out.push('\n');
        out.push_str(&field(
            "context",
            &format!(
                "{} tokens  (in {} · cache +{} ·{})",
                u.context_tokens(),
                u.input_tokens,
                u.cache_creation_input_tokens,
                u.cache_read_input_tokens,
            ),
        ));
        out.push_str(&field("output", &format!("{} tokens", u.output_tokens)));
        out.push_str(&field("turns", &u.num_turns.to_string()));
        if let Some(cost) = u.cost_usd {
            out.push_str(&field("cost", &format!("${cost:.4}")));
        }
    }

    // Conversation summary.
    out.push('\n');
    out.push_str(&paint(color, SGR_BOLD, "conversation"));
    out.push('\n');
    out.push_str(&format!("  {} messages\n", summary.message_count));

    // Pending permissions, each with the short id `approve`/`deny --request`
    // takes. Omitted entirely when nothing is waiting.
    if !summary.pending.is_empty() {
        out.push('\n');
        out.push_str(&paint(color, SGR_NEEDS_INPUT, "pending permissions"));
        out.push('\n');
        for p in &summary.pending {
            let question = if p.is_question { "  (question)" } else { "" };
            out.push_str(&format!(
                "  {}  {}  {}{question}\n",
                short_perm_id(&p.perm_id),
                col(&p.tool_name, 16),
                relative_time(now, p.created_ms / 1000),
            ));
        }
    }

    render_subagents(&mut out, &summary.subagents, color);

    out
}

/// Append the subagent rollup to `out`: a tally line (e.g. `3 completed, 1
/// running (1 background), 0 failed`) then one `type  description  status`
/// line per subagent, in spawn order. Appends nothing when `subagents` is
/// empty.
fn render_subagents(out: &mut String, subagents: &[SubagentSummary], color: bool) {
    if subagents.is_empty() {
        return;
    }
    let c = SubagentCounts::tally(subagents);
    let background = if c.background > 0 {
        format!(" ({} background)", c.background)
    } else {
        String::new()
    };

    out.push('\n');
    out.push_str(&paint(color, SGR_BOLD, "subagents"));
    out.push('\n');
    out.push_str(&format!(
        "  {} completed, {} running{background}, {} failed\n",
        c.completed, c.running, c.failed
    ));
    for s in subagents {
        let bg = if s.background { "  (background)" } else { "" };
        out.push_str(&format!(
            "  {}  {}  {}{bg}\n",
            col(&s.subagent_type, 16),
            col(&s.description, 48),
            s.status.as_wire(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::super::list::tests::session;
    use super::*;
    use crate::config::RunConfig;
    use crate::messages::UsageInfo;

    fn usage(input: u64, cc: u64, cr: u64, out: u64, turns: u32, cost: Option<f64>) -> UsageInfo {
        UsageInfo {
            input_tokens: input,
            cache_creation_input_tokens: cc,
            cache_read_input_tokens: cr,
            output_tokens: out,
            cost_usd: cost,
            num_turns: turns,
        }
    }

    #[test]
    fn render_detail_plain_has_every_section() {
        let s = session("mac", "My Session", "working", 0);
        let configs = vec![RunConfig::new("build".into(), "cargo build".into())];
        let u = usage(100, 10, 20, 50, 3, Some(0.25));
        let summary = ConversationSummary {
            message_count: 7,
            pending: vec![PendingPermission {
                perm_id: uuid::Uuid::parse_str("aaaa1111-0000-0000-0000-000000000000").unwrap(),
                tool_name: "Bash".into(),
                created_ms: 0,
                is_question: false,
            }],
            subagents: vec![],
        };
        let out = render_detail(&s, &configs, Some(&u), &summary, 60, false);

        assert!(!out.contains('\x1b'), "no ANSI when color=false: {out:?}");
        // header + state fields
        assert!(out.contains("agentium:"));
        assert!(out.contains("● Working"));
        assert!(out.contains("My Session"));
        assert!(out.contains("mac-My Session")); // claude_session_id
        assert!(out.contains("~/proj")); // cwd home-abbreviated
        assert!(out.contains("1m ago (0)")); // created relative + raw ts
                                             // run configs
        assert!(out.contains("run configs"));
        assert!(out.contains("cargo build"));
        // usage: context = input + both cache buckets
        assert!(out.contains("usage"));
        assert!(out.contains("130 tokens"));
        assert!(out.contains("$0.2500"));
        assert!(out.contains("3")); // turns
                                    // conversation summary + the pending request, with the short id
                                    // `approve --request` takes
        assert!(out.contains("7 messages"));
        assert!(out.contains("pending permissions"));
        assert!(out.contains("aaaa1111  Bash"), "{out}");
    }

    /// The issue line shows only when the session was spawned with one, like
    /// the indicator — most sessions have none, so a dash row would be noise.
    #[test]
    fn render_detail_shows_issue_only_when_set() {
        let summary = ConversationSummary {
            message_count: 0,
            pending: vec![],
            subagents: vec![],
        };
        let mut s = session("mac", "t", "idle", 0);
        let out = render_detail(&s, &[], None, &summary, 0, false);
        assert!(!out.contains("issue"), "{out}");

        s.issue_url = Some("headway:dave/receive-east-neutral".into());
        let out = render_detail(&s, &[], None, &summary, 0, false);
        assert!(out.contains("headway:dave/receive-east-neutral"), "{out}");
    }

    /// A session under an agent spec names it in the text and in `--json`
    /// (`agent_spec`, flattened from the state); one under none shows no
    /// line and a `null`.
    #[test]
    fn show_names_the_agent_spec_in_text_and_json() {
        let summary = ConversationSummary {
            message_count: 0,
            pending: vec![],
            subagents: vec![],
        };
        let mut detail = SessionDetail {
            state: session("mac", "t", "idle", 0),
            run_configs: vec![],
            usage: None,
            summary,
        };
        assert!(!detail.render(0, false).contains("agent spec"));
        let json = serde_json::to_value(detail.json()).unwrap();
        assert!(json["session"]["agent_spec"].is_null(), "{json}");

        detail.state.agent_spec = Some("butler".into());
        let out = detail.render(0, false);
        assert!(
            out.lines()
                .any(|l| l.contains("agent spec") && l.contains("butler")),
            "{out}"
        );
        let json = serde_json::to_value(detail.json()).unwrap();
        assert_eq!(json["session"]["agent_spec"], "butler", "{json}");
    }

    /// The parent and relation lines show only when the spawn recorded them,
    /// like the issue line.
    #[test]
    fn render_detail_shows_lineage_only_when_set() {
        let summary = ConversationSummary {
            message_count: 0,
            pending: vec![],
            subagents: vec![],
        };
        let mut s = session("mac", "t", "idle", 0);
        let out = render_detail(&s, &[], None, &summary, 0, false);
        assert!(!out.contains("parent"), "{out}");
        assert!(!out.contains("relation"), "{out}");

        s.parent_session = Some("agentium:the-parent-ref".into());
        s.relation = crate::session_events::SessionRelation::Child;
        let out = render_detail(&s, &[], None, &summary, 0, false);
        assert!(out.contains("agentium:the-parent-ref"), "{out}");
        assert!(
            out.lines()
                .any(|l| l.contains("relation") && l.contains("child")),
            "{out}"
        );
    }

    #[test]
    fn render_detail_omits_usage_section_when_absent() {
        let s = session("mac", "t", "idle", 0);
        let summary = ConversationSummary {
            message_count: 0,
            pending: vec![],
            subagents: vec![],
        };
        let out = render_detail(&s, &[], None, &summary, 0, false);
        assert!(
            !out.contains("usage"),
            "usage section hidden when None: {out:?}"
        );
        assert!(out.contains("run configs"));
        assert!(out.contains("  none")); // no configs registered
        assert!(out.contains("0 messages"));
        assert!(!out.contains("pending permissions"));
        assert!(
            !out.contains("subagents"),
            "subagents section hidden when none: {out:?}"
        );
    }

    fn subagent(id: &str, ty: &str, status: SubagentStatus, background: bool) -> SubagentSummary {
        SubagentSummary {
            task_id: id.into(),
            subagent_type: ty.into(),
            description: format!("do {id}"),
            status,
            background,
        }
    }

    /// The four-subagent session the card's example line describes: three
    /// completed, one running in the background.
    fn rollup() -> Vec<SubagentSummary> {
        vec![
            subagent("t1", "Explore", SubagentStatus::Completed, false),
            subagent("t2", "Plan", SubagentStatus::Completed, false),
            subagent("t3", "Explore", SubagentStatus::Running, true),
            subagent("t4", "general-purpose", SubagentStatus::Completed, false),
        ]
    }

    #[test]
    fn render_detail_rolls_up_subagents() {
        let s = session("mac", "t", "working", 0);
        let summary = ConversationSummary {
            message_count: 4,
            pending: vec![],
            subagents: rollup(),
        };
        let out = render_detail(&s, &[], None, &summary, 0, false);

        assert!(out.contains("subagents"), "{out}");
        assert!(
            out.contains("3 completed, 1 running (1 background), 0 failed"),
            "{out}"
        );
        // One line per subagent, in spawn order, the background one flagged.
        let rows: Vec<&str> = out
            .lines()
            .skip_while(|l| *l != "subagents")
            .skip(2)
            .collect();
        assert_eq!(rows.len(), 4, "{out}");
        assert!(rows[0].contains("Explore") && rows[0].contains("do t1"));
        assert!(rows[0].ends_with("completed"), "{:?}", rows[0]);
        assert!(rows[2].contains("do t3") && rows[2].ends_with("running  (background)"));
        assert!(rows[3].contains("general-purpose"));
    }

    #[test]
    fn subagents_json_counts_and_items() {
        let json = serde_json::to_value(SubagentsJson::from(&rollup()[..])).unwrap();
        assert_eq!(json["running"], 1);
        assert_eq!(json["completed"], 3);
        assert_eq!(json["failed"], 0);
        assert_eq!(json["background"], 1);
        assert_eq!(json["items"].as_array().unwrap().len(), 4);
        assert_eq!(
            json["items"][2],
            serde_json::json!({
                "task_id": "t3",
                "subagent_type": "Explore",
                "description": "do t3",
                "status": "running",
                "background": true,
            })
        );

        let empty = serde_json::to_value(SubagentsJson::from(&[][..])).unwrap();
        assert_eq!(
            empty,
            serde_json::json!({
                "running": 0, "completed": 0, "failed": 0, "background": 0, "items": [],
            })
        );
    }

    #[test]
    fn pending_permissions_json_lists_every_request() {
        let summary = ConversationSummary {
            message_count: 2,
            pending: vec![
                PendingPermission {
                    perm_id: uuid::Uuid::parse_str("aaaa1111-0000-0000-0000-000000000000").unwrap(),
                    tool_name: "Bash".into(),
                    created_ms: 5_500,
                    is_question: false,
                },
                PendingPermission {
                    perm_id: uuid::Uuid::parse_str("bbbb2222-0000-0000-0000-000000000000").unwrap(),
                    tool_name: "AskUserQuestion".into(),
                    created_ms: 7_000,
                    is_question: true,
                },
            ],
            subagents: vec![],
        };
        let json = serde_json::to_value(ConversationJson::from(&summary)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "message_count": 2,
                "pending_permissions": [
                    {
                        "perm_id": "aaaa1111-0000-0000-0000-000000000000",
                        "tool_name": "Bash",
                        "created_at": 5,
                        "is_question": false,
                    },
                    {
                        "perm_id": "bbbb2222-0000-0000-0000-000000000000",
                        "tool_name": "AskUserQuestion",
                        "created_at": 7,
                        "is_question": true,
                    },
                ],
            })
        );
    }
}
