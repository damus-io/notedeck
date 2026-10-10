//! The session inspector's snapshot: what a host publishes so a session
//! observed from another device (or read by the `agentium` CLI) shows the same
//! Details tab, apart from file contents.
//!
//! A host session's inspector state — usage, rate limits, background work,
//! subagent totals, its temp files and its commits — lives only in the host's
//! memory, fed by the backend's stream or read from the host's disk and git.
//! Nothing of it reaches an observer through the transcript. So the host
//! publishes an [`InspectorSnapshot`] as its own parameterized-replaceable
//! event, kind [`AI_SESSION_DETAILS_KIND`], one per session (`d` = the
//! session's kind-31988 d-tag), PNS-wrapped like every other session note. It
//! is kept out of the kind-31988 state event, which every session-list fold
//! reads and which is fat enough already.
//!
//! What it leaves out, because observers have it already or must not have it:
//! the task list and permission-prompt counts (both fold from the transcript),
//! output tails and file contents (host-local), and the host's always-allow
//! rules (per device: a viewer's own rules are what answer prompts from it).
//!
//! The plain data types here are the inspector's own, shared with
//! `notedeck_dave`'s Details tab so the wire shape and the state are one type.

use crate::messages::{
    BackgroundEntry, CliState, RateLimitInfo, SessionInfo, SubagentProgress, UsageInfo,
};
use crate::session_events::{
    build_within_wire_budget, finalize_built_event, get_tag_value, init_note_builder, BuiltEvent,
    EventBuildError,
};
use nostrdb::{Filter, Ndb, Note, Transaction};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Nostr event kind for a session's inspector snapshot (parameterized
/// replaceable, NIP-33). One event per session, `d` = the session's kind-31988
/// d-tag, content = the [`InspectorSnapshot`] as JSON.
pub const AI_SESSION_DETAILS_KIND: u32 = 31990;

/// The most entries any one list in a snapshot carries: turns, compactions,
/// background entries, subagents, files per group, commits, and each of the
/// init lists. A snapshot over the wire budget is cut to fewer.
pub const SNAPSHOT_LIST_CAP: usize = 100;

/// One request's token counts: what occupied the context and what the model
/// wrote back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tokens {
    /// Uncached input tokens.
    pub input: u64,
    /// Input tokens written to the prompt cache.
    pub cache_creation: u64,
    /// Input tokens read from the prompt cache.
    pub cache_read: u64,
    pub output: u64,
}

impl Tokens {
    /// The counts one usage report carries.
    pub fn from_usage(usage: &UsageInfo) -> Self {
        Self {
            input: usage.input_tokens,
            cache_creation: usage.cache_creation_input_tokens,
            cache_read: usage.cache_read_input_tokens,
            output: usage.output_tokens,
        }
    }

    /// Tokens occupying the context window: all three kinds of input.
    pub fn context(&self) -> u64 {
        self.input + self.cache_creation + self.cache_read
    }
}

/// Where one turn left the session: its last request's tokens, and what the
/// turn cost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TurnUsage {
    /// When the turn ended, in Unix seconds.
    pub at: u64,
    /// The turn's last request, whose input is the context at turn end.
    pub tokens: Tokens,
    /// What this turn added to the session's cost, if the backend reports
    /// cost.
    pub cost_usd: Option<f64>,
}

/// The CLI compacted the conversation between two turns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Compaction {
    /// When it finished, in Unix seconds.
    pub at: u64,
    /// How many turns the session had recorded when it happened, so it sits
    /// just before turn number `turn` (counted from the session's first,
    /// including turns the cap has since dropped).
    pub turn: u64,
    /// The context it started from.
    pub pre_tokens: u64,
    /// The context afterwards, from the first request after it. `None` until
    /// that request arrives.
    pub post_tokens: Option<u64>,
}

/// One entry of a temp-dir group.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TempFile {
    /// The file name, lossily decoded.
    pub name: String,
    pub path: PathBuf,
    /// Bytes; zero for a directory.
    pub size: u64,
    /// Last modified, in Unix seconds; zero if unknown.
    pub modified: u64,
    /// A directory rather than a file (the scratchpad may hold some).
    pub is_dir: bool,
}

/// One subdirectory's entries, newest first, at most a capped number of them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TempGroup {
    pub files: Vec<TempFile>,
    /// How many entries the directory holds, listed or not.
    pub total: usize,
}

/// What a session's Claude Code temp dir held when it was last read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TempListing {
    /// The session's temp dir.
    pub dir: PathBuf,
    /// `scratchpad/`.
    pub scratchpad: TempGroup,
    /// `tasks/`: background-task output logs.
    pub tasks: TempGroup,
}

/// One commit carrying the session's trailer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    /// The full hash, which a click copies.
    pub hash: String,
    /// Git's abbreviated hash, as the card shows it.
    pub short: String,
    /// The subject line.
    pub subject: String,
}

/// What `git log` answered: the session's commits newest first, or why there
/// are none to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitList {
    Commits(Vec<Commit>),
    /// The cwd isn't in a git repository, or doesn't exist.
    NotARepo,
    /// git failed some other way; its stderr.
    Failed(String),
}

/// A session's usage history as the host holds it: the live context, the
/// session's totals, and the newest turns and compactions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageSnapshot {
    /// The latest request's tokens: the context right now.
    pub current: Option<Tokens>,
    /// The newest turns, oldest first.
    pub turns: Vec<TurnUsage>,
    /// The newest compactions, oldest first.
    pub compactions: Vec<Compaction>,
    /// Every turn the session recorded, including ones not carried here.
    pub turns_recorded: u64,
    /// Model requests across every recorded turn.
    pub requests: u64,
    /// What the session has cost, if the backend reports cost.
    pub total_cost: Option<f64>,
}

/// What a host publishes about one session for observers' Details tab. See
/// the module docs for what it leaves out.
///
/// Every field defaults, so a snapshot from an older or newer host parses
/// with whatever the two have in common.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InspectorSnapshot {
    /// The CLI's latest `system/init`.
    pub session_info: Option<SessionInfo>,
    /// Whether the host's CLI process is running, and when it will be reaped.
    pub cli: Option<CliState>,
    pub usage: UsageSnapshot,
    /// The account's rate-limit windows, as the host last heard them.
    pub rate_limit: Option<RateLimitInfo>,
    /// Background tasks, wake-ups and crons, newest first.
    pub background: Vec<BackgroundEntry>,
    /// Each subagent's latest totals.
    pub subagents: Vec<SubagentProgress>,
    /// The session's temp dir: names, sizes and times, never contents.
    pub files: Option<TempListing>,
    /// The commits carrying the session's `Agentium:` trailer.
    pub commits: Option<CommitList>,
}

impl InspectorSnapshot {
    /// This snapshot with every list cut to at most `cap` entries: the
    /// newest turns and compactions, the first of everything else. Counts
    /// (`total`, `turns_recorded`) keep what the host has.
    pub fn capped(&self, cap: usize) -> InspectorSnapshot {
        fn first<T: Clone>(items: &[T], cap: usize) -> Vec<T> {
            items[..items.len().min(cap)].to_vec()
        }
        fn last<T: Clone>(items: &[T], cap: usize) -> Vec<T> {
            items[items.len().saturating_sub(cap)..].to_vec()
        }
        let group = |g: &TempGroup| TempGroup {
            files: first(&g.files, cap),
            total: g.total,
        };
        InspectorSnapshot {
            session_info: self.session_info.as_ref().map(|info| SessionInfo {
                tools: first(&info.tools, cap),
                slash_commands: first(&info.slash_commands, cap),
                agents: first(&info.agents, cap),
                mcp_servers: first(&info.mcp_servers, cap),
                skills: first(&info.skills, cap),
                plugins: first(&info.plugins, cap),
                plugin_errors: first(&info.plugin_errors, cap),
                betas: first(&info.betas, cap),
                ..info.clone()
            }),
            cli: self.cli,
            usage: UsageSnapshot {
                turns: last(&self.usage.turns, cap),
                compactions: last(&self.usage.compactions, cap),
                ..self.usage.clone()
            },
            rate_limit: self.rate_limit,
            background: first(&self.background, cap),
            subagents: first(&self.subagents, cap),
            files: self.files.as_ref().map(|listing| TempListing {
                dir: listing.dir.clone(),
                scratchpad: group(&listing.scratchpad),
                tasks: group(&listing.tasks),
            }),
            commits: self.commits.as_ref().map(|list| match list {
                CommitList::Commits(commits) => CommitList::Commits(first(commits, cap)),
                other => other.clone(),
            }),
        }
    }

    /// Build the kind-[`AI_SESSION_DETAILS_KIND`] event for session
    /// `session_id`, stamped `created_at`, with its lists cut as far as it
    /// takes to fit the wire budget.
    pub fn build_event(
        &self,
        session_id: &str,
        created_at: u64,
        secret_key: &[u8; 32],
    ) -> Result<BuiltEvent, EventBuildError> {
        let built = build_within_wire_budget(SNAPSHOT_LIST_CAP, |cap| {
            let content = serde_json::to_string(&self.capped(cap))
                .map_err(|e| EventBuildError::Serialize(e.to_string()))?;
            let builder = init_note_builder(AI_SESSION_DETAILS_KIND, &content, Some(created_at))
                .start_tag()
                .tag_str("d")
                .tag_str(session_id);
            finalize_built_event(builder, secret_key, AI_SESSION_DETAILS_KIND)
        })?;
        built.ok_or(EventBuildError::OverWireBudget)
    }

    /// The snapshot a kind-[`AI_SESSION_DETAILS_KIND`] note carries, or
    /// `None` for another kind or content that doesn't parse.
    pub fn from_note(note: &Note) -> Option<InspectorSnapshot> {
        if note.kind() != AI_SESSION_DETAILS_KIND {
            return None;
        }
        serde_json::from_str(note.content()).ok()
    }
}

/// A session's newest snapshot and when it was published.
#[derive(Debug, Clone, PartialEq)]
pub struct PublishedSnapshot {
    /// The note's `created_at`, to order revisions by.
    pub created_at: u64,
    pub snapshot: InspectorSnapshot,
}

/// The newest snapshot `author` published for session `session_id` (its
/// kind-31988 d-tag), if ndb has one.
pub fn latest_snapshot(
    ndb: &Ndb,
    txn: &Transaction,
    author: &[u8; 32],
    session_id: &str,
) -> Option<PublishedSnapshot> {
    let filter = Filter::new()
        .kinds([AI_SESSION_DETAILS_KIND as u64])
        .authors([author])
        .tags([session_id], 'd')
        .limit(1)
        .build();
    let results = ndb.query(txn, &[filter], 1).ok()?;
    let note = &results.first()?.note;
    Some(PublishedSnapshot {
        created_at: note.created_at(),
        snapshot: InspectorSnapshot::from_note(note)?,
    })
}

/// The session a snapshot note is about: its `d` tag.
pub fn snapshot_session_id<'a>(note: &'a Note<'a>) -> Option<&'a str> {
    get_tag_value(note, "d")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{BackgroundTask, BackgroundWork, RateLimitStatus, RateLimitWindow};

    fn sample() -> InspectorSnapshot {
        InspectorSnapshot {
            session_info: Some(SessionInfo {
                model: Some("claude-opus-5-5".into()),
                tools: vec!["Bash".into(), "Read".into()],
                ..Default::default()
            }),
            cli: Some(CliState {
                connected: true,
                reap_at: Some(42),
            }),
            usage: UsageSnapshot {
                current: Some(Tokens {
                    input: 1,
                    cache_read: 1000,
                    ..Default::default()
                }),
                turns: (0..5)
                    .map(|i| TurnUsage {
                        at: i,
                        cost_usd: Some(0.5),
                        ..Default::default()
                    })
                    .collect(),
                turns_recorded: 5,
                requests: 9,
                total_cost: Some(2.5),
                ..Default::default()
            },
            rate_limit: Some(RateLimitInfo {
                status: RateLimitStatus::AllowedWarning,
                five_hour: Some(RateLimitWindow {
                    utilization: 0.82,
                    resets_at: 1_791_590_400,
                }),
                ..Default::default()
            }),
            background: vec![
                BackgroundEntry {
                    id: "b1".into(),
                    started_at: 7,
                    work: BackgroundWork::Task(BackgroundTask {
                        description: "cargo test".into(),
                        output_file: Some("/tmp/x/tasks/b1.output".into()),
                        ..Default::default()
                    }),
                },
                BackgroundEntry {
                    id: "wakeup".into(),
                    started_at: 8,
                    work: BackgroundWork::Wakeup {
                        due_at: 99,
                        reason: "ci".into(),
                        prompt: "check".into(),
                    },
                },
            ],
            subagents: vec![SubagentProgress {
                task_id: "toolu_1".into(),
                ..Default::default()
            }],
            files: Some(TempListing {
                dir: "/tmp/claude-1000/x/sid".into(),
                scratchpad: TempGroup {
                    files: vec![TempFile {
                        name: "notes.md".into(),
                        size: 12,
                        ..Default::default()
                    }],
                    total: 1,
                },
                ..Default::default()
            }),
            commits: Some(CommitList::Commits(vec![Commit {
                hash: "abc".into(),
                short: "ab".into(),
                subject: "dave: a thing".into(),
            }])),
        }
    }

    /// The JSON the event carries reads back as the snapshot it was built
    /// from.
    #[test]
    fn snapshot_round_trips_through_json() {
        let snapshot = sample();
        let json = serde_json::to_string(&snapshot).unwrap();
        let back: InspectorSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, snapshot);
    }

    /// A snapshot from a host that knows fields this one doesn't, or lacks
    /// ones it has, still parses.
    #[test]
    fn snapshot_tolerates_missing_and_unknown_fields() {
        let back: InspectorSnapshot =
            serde_json::from_str(r#"{"usage":{"requests":3},"later_field":1}"#).unwrap();
        assert_eq!(back.usage.requests, 3);
        assert_eq!(back.background, []);
    }

    /// Capping keeps the newest turns and the first of everything else.
    #[test]
    fn capped_keeps_newest_turns_and_counts() {
        let capped = sample().capped(2);
        let ats: Vec<u64> = capped.usage.turns.iter().map(|t| t.at).collect();
        assert_eq!(ats, [3, 4]);
        assert_eq!(capped.usage.turns_recorded, 5);
        assert_eq!(capped.background.len(), 2);
        assert_eq!(capped.capped(1).background[0].id, "b1");
    }

    /// A snapshot too big for the wire is cut until it fits, not dropped.
    #[test]
    fn build_event_cuts_an_oversized_snapshot_to_fit() {
        let mut snapshot = sample();
        snapshot.session_info.as_mut().unwrap().tools = (0..SNAPSHOT_LIST_CAP)
            .map(|i| {
                format!(
                    "mcp__server__a_rather_long_tool_name_{i}_{}",
                    "x".repeat(400)
                )
            })
            .collect();
        let sk = [7u8; 32];
        let event = snapshot.build_event("sid", 100, &sk).unwrap();
        assert!(event.note_json.len() <= crate::session_events::MAX_WIRE_EVENT_BYTES);
        assert_eq!(event.kind, AI_SESSION_DETAILS_KIND);
    }
}
