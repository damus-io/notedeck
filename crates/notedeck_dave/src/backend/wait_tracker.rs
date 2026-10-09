//! Tracks what a Claude session has started that will resume it after its
//! foreground turn ends, from the CLI's own message stream.
//!
//! Three kinds of thing wake a session:
//!
//! - **Background tasks** — a `run_in_background` Bash command, a Monitor, a
//!   background subagent. The CLI reports these itself:
//!   `background_tasks_changed` carries the full set of live tasks on every
//!   change, and the older `task_started` / `task_notification` pair bookends
//!   each one. The level is authoritative once seen; the bookends are the
//!   fallback for a CLI that never sends it. The two key tasks differently
//!   (task id vs the launching `tool_use_id`), so they are never mixed.
//! - **Scheduled wake-ups** — `ScheduleWakeup` resumes the session after a
//!   delay. The CLI sends nothing when one is armed or fires, so it is read
//!   from the tool call and expires at its due time.
//! - **Crons** — `CronCreate` / `CronDelete`. A recurring job holds until it
//!   is deleted. A one-shot job holds until a wake-up turn that no background
//!   task explains, which is the job firing.
//!
//! Not visible here: a wake-up the CLI arms on its own (the `/loop` keepalive
//! fallback when the model does not reschedule), and a durable cron that
//! outlives the CLI. Both resume the session without a tool call in the
//! stream.

use super::claude::parse_user_content_blocks;
use crate::messages::WaitingOn;
use claude_agent_sdk_rs::{
    ContentBlock, Message as ClaudeMessage, ToolResultContent, ToolUseBlock,
};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::time::Instant;

/// The CLI clamps a `ScheduleWakeup` delay to this range (seconds).
const WAKEUP_DELAY_RANGE: (u64, u64) = (60, 3600);

/// A tool call that changes what is scheduled, held from its `tool_use` until
/// its result says whether it took effect.
enum Scheduling {
    /// `ScheduleWakeup` arming a wake-up `delay_secs` from its result.
    Wakeup { delay_secs: u64 },
    /// `ScheduleWakeup { stop: true }`: the loop ends, nothing is armed.
    StopWakeup,
    /// `CronCreate`; the job id and kind are in its result text.
    CronCreate,
    /// `CronDelete` of the job `id`.
    CronDelete { id: String },
}

impl Scheduling {
    /// The scheduling a tool call makes, if it makes one.
    fn from_tool_use(name: &str, input: &serde_json::Value) -> Option<Self> {
        match name {
            "ScheduleWakeup" => {
                if input.get("stop").and_then(serde_json::Value::as_bool) == Some(true) {
                    return Some(Self::StopWakeup);
                }
                let delay = input
                    .get("delaySeconds")
                    .and_then(serde_json::Value::as_f64)?;
                let (lo, hi) = WAKEUP_DELAY_RANGE;
                Some(Self::Wakeup {
                    delay_secs: (delay.max(0.0) as u64).clamp(lo, hi),
                })
            }
            "CronCreate" => Some(Self::CronCreate),
            "CronDelete" => {
                let id = input.get("id").and_then(serde_json::Value::as_str)?;
                Some(Self::CronDelete { id: id.to_string() })
            }
            _ => None,
        }
    }
}

/// An armed `ScheduleWakeup`, in both clocks: Unix seconds for the wire, an
/// [`Instant`] for the idle reaper.
#[derive(Clone, Copy)]
struct Wakeup {
    at_unix: u64,
    at: Instant,
}

/// Everything outstanding that will resume the session. See the module docs.
#[derive(Default)]
pub(crate) struct WaitTracker {
    /// The live background tasks as of the CLI's last
    /// `background_tasks_changed`, by task id. `None` until the CLI sends one.
    level_tasks: Option<HashSet<String>>,
    /// Background tasks between `task_started` and `task_notification`, by
    /// launching `tool_use_id`. Only consulted while `level_tasks` is `None`.
    edge_tasks: HashSet<String>,
    /// Scheduling tool calls whose results have not arrived, by `tool_use_id`.
    scheduling: HashMap<String, Scheduling>,
    wakeup: Option<Wakeup>,
    recurring_crons: HashSet<String>,
    one_shot_crons: HashSet<String>,
    /// A background task ended since the last turn did, so the next wake-up
    /// turn is that task's notification and not a cron firing.
    task_ended_since_turn: bool,
    /// What was last reported, so only changes are sent.
    reported: WaitingOn,
}

impl WaitTracker {
    /// Account for one message from the CLI. `now` / `now_unix` are the same
    /// moment in both clocks.
    pub(crate) fn on_message(&mut self, now: Instant, now_unix: u64, message: &ClaudeMessage) {
        match message {
            ClaudeMessage::System(system_msg) => {
                self.on_system(&system_msg.subtype, &system_msg.data)
            }
            ClaudeMessage::Assistant(assistant_msg) => {
                for block in &assistant_msg.message.content {
                    if let ContentBlock::ToolUse(ToolUseBlock { id, name, input }) = block {
                        if let Some(scheduling) = Scheduling::from_tool_use(name, input) {
                            self.scheduling.insert(id.clone(), scheduling);
                        }
                    }
                }
            }
            ClaudeMessage::User(user_msg) => {
                for block in parse_user_content_blocks(user_msg) {
                    let ContentBlock::ToolResult(result) = block else {
                        continue;
                    };
                    let Some(scheduling) = self.scheduling.remove(&result.tool_use_id) else {
                        continue;
                    };
                    if result.is_error == Some(true) {
                        continue;
                    }
                    self.apply(scheduling, result_text(&result.content), now, now_unix);
                }
            }
            ClaudeMessage::Result(_) => self.task_ended_since_turn = false,
            _ => {}
        }
    }

    /// A background-task system message.
    fn on_system(&mut self, subtype: &str, data: &serde_json::Value) {
        match subtype {
            "background_tasks_changed" => {
                let Some(tasks) = data.get("tasks").and_then(serde_json::Value::as_array) else {
                    return;
                };
                // An ambient task (a watcher nobody asked for) is not activity
                // and wakes nothing on the session's behalf.
                let live: HashSet<String> = tasks
                    .iter()
                    .filter(|t| t.get("ambient").and_then(serde_json::Value::as_bool) != Some(true))
                    .filter_map(|t| t.get("task_id").and_then(serde_json::Value::as_str))
                    .map(str::to_string)
                    .collect();
                if let Some(previous) = &self.level_tasks {
                    if previous.iter().any(|id| !live.contains(id)) {
                        self.task_ended_since_turn = true;
                    }
                }
                self.level_tasks = Some(live);
            }
            "task_started" => {
                if let Some(id) = tool_use_id(data) {
                    self.edge_tasks.insert(id.to_string());
                }
            }
            "task_notification" => {
                self.task_ended_since_turn = true;
                if let Some(id) = tool_use_id(data) {
                    self.edge_tasks.remove(id);
                }
            }
            _ => {}
        }
    }

    /// A scheduling tool call succeeded.
    fn apply(&mut self, scheduling: Scheduling, result: Option<&str>, now: Instant, now_unix: u64) {
        match scheduling {
            Scheduling::Wakeup { delay_secs } => {
                self.wakeup = Some(Wakeup {
                    at_unix: now_unix + delay_secs,
                    at: now + Duration::from_secs(delay_secs),
                });
            }
            Scheduling::StopWakeup => self.wakeup = None,
            Scheduling::CronCreate => match result.and_then(parse_cron_created) {
                Some(CronCreated {
                    id,
                    recurring: true,
                }) => {
                    self.recurring_crons.insert(id);
                }
                Some(CronCreated {
                    id,
                    recurring: false,
                }) => {
                    self.one_shot_crons.insert(id);
                }
                None => tracing::debug!("CronCreate result without a job id: {result:?}"),
            },
            Scheduling::CronDelete { id } => {
                self.recurring_crons.remove(&id);
                self.one_shot_crons.remove(&id);
            }
        }
    }

    /// A turn started that no query began. If no background task ended to
    /// explain it, something scheduled fired: the wake-up if it is due, else a
    /// one-shot cron (which deletes itself once fired).
    pub(crate) fn wakeup_turn_started(&mut self, now_unix: u64) {
        if std::mem::take(&mut self.task_ended_since_turn) {
            return;
        }
        // The CLI fires on its own clock; allow it to run a little early.
        const DUE_SLACK_SECS: u64 = 60;
        match self.wakeup {
            Some(wakeup) if wakeup.at_unix <= now_unix + DUE_SLACK_SECS => self.wakeup = None,
            _ => self.one_shot_crons.clear(),
        }
    }

    /// The CLI is gone, and everything it would have run with it.
    pub(crate) fn clear(&mut self) {
        let reported = self.reported;
        *self = Self {
            reported,
            ..Self::default()
        };
    }

    /// Live background tasks.
    fn background_tasks(&self) -> usize {
        self.level_tasks.as_ref().unwrap_or(&self.edge_tasks).len()
    }

    /// Whether something outstanding lives in the CLI process and would die
    /// with it: a background task or a cron. Its due wake-up is separate
    /// (see [`wakeup_at`](Self::wakeup_at)).
    pub(crate) fn holds_cli(&self) -> bool {
        self.background_tasks() > 0
            || !self.recurring_crons.is_empty()
            || !self.one_shot_crons.is_empty()
    }

    /// When the armed wake-up is due, if one is.
    pub(crate) fn wakeup_at(&self) -> Option<Instant> {
        self.wakeup.map(|w| w.at)
    }

    /// The current state, as the UI sees it.
    pub(crate) fn waiting_on(&self) -> WaitingOn {
        WaitingOn {
            background_tasks: u32::try_from(self.background_tasks()).unwrap_or(u32::MAX),
            wakeup_at: self.wakeup.map(|w| w.at_unix),
            crons: u32::try_from(self.recurring_crons.len() + self.one_shot_crons.len())
                .unwrap_or(u32::MAX),
        }
    }

    /// The current state if it changed since the last call, to report.
    pub(crate) fn take_change(&mut self) -> Option<WaitingOn> {
        let now = self.waiting_on();
        if now == self.reported {
            return None;
        }
        self.reported = now;
        Some(now)
    }
}

/// The launching `tool_use_id` a task bookend carries.
fn tool_use_id(data: &serde_json::Value) -> Option<&str> {
    data.get("tool_use_id").and_then(serde_json::Value::as_str)
}

/// A tool result's text, when it is plain text.
fn result_text(content: &Option<ToolResultContent>) -> Option<&str> {
    match content {
        Some(ToolResultContent::Text(text)) => Some(text),
        Some(ToolResultContent::Blocks(blocks)) => blocks
            .iter()
            .find_map(|b| b.get("text").and_then(serde_json::Value::as_str)),
        None => None,
    }
}

/// A cron job `CronCreate` scheduled.
#[derive(Debug, PartialEq, Eq)]
struct CronCreated {
    id: String,
    recurring: bool,
}

/// Read the job out of a `CronCreate` result: "Scheduled recurring job <id>
/// (...)" or "Scheduled one-shot task <id> (...)".
fn parse_cron_created(text: &str) -> Option<CronCreated> {
    let (rest, recurring) = if let Some(rest) = text.strip_prefix("Scheduled recurring job ") {
        (rest, true)
    } else {
        (text.strip_prefix("Scheduled one-shot task ")?, false)
    };
    let id = rest.split_whitespace().next()?;
    Some(CronCreated {
        id: id.to_string(),
        recurring,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const T0_UNIX: u64 = 1_800_000_000;

    fn wire(message: serde_json::Value) -> ClaudeMessage {
        serde_json::from_value(message).expect("message should deserialize")
    }

    fn system(subtype: &str, data: serde_json::Value) -> ClaudeMessage {
        let mut message = json!({ "type": "system", "subtype": subtype });
        message
            .as_object_mut()
            .unwrap()
            .extend(data.as_object().unwrap().clone());
        wire(message)
    }

    fn tool_use(id: &str, name: &str, input: serde_json::Value) -> ClaudeMessage {
        wire(json!({
            "type": "assistant",
            "message": { "content": [{ "type": "tool_use", "id": id, "name": name, "input": input }] }
        }))
    }

    fn tool_result(id: &str, text: &str) -> ClaudeMessage {
        wire(json!({
            "type": "user",
            "message": { "content": [{ "type": "tool_result", "tool_use_id": id, "content": text }] }
        }))
    }

    fn turn_result() -> ClaudeMessage {
        wire(json!({
            "type": "result",
            "subtype": "success",
            "duration_ms": 1,
            "duration_api_ms": 1,
            "is_error": false,
            "num_turns": 1,
            "session_id": "sess-1"
        }))
    }

    fn feed(tracker: &mut WaitTracker, messages: &[ClaudeMessage]) {
        let now = Instant::now();
        for message in messages {
            tracker.on_message(now, T0_UNIX, message);
        }
    }

    /// A background Bash task holds the session waiting from its
    /// `task_started` until its `task_notification`, on a CLI that sends only
    /// the bookends.
    #[test]
    fn bookends_track_a_background_task() {
        let mut tracker = WaitTracker::default();
        feed(
            &mut tracker,
            &[system(
                "task_started",
                json!({ "task_type": "local_bash", "tool_use_id": "toolu_bg" }),
            )],
        );
        assert_eq!(tracker.waiting_on().background_tasks, 1);
        assert!(tracker.holds_cli());

        feed(
            &mut tracker,
            &[system(
                "task_notification",
                json!({ "tool_use_id": "toolu_bg", "status": "completed" }),
            )],
        );
        assert_eq!(tracker.waiting_on(), WaitingOn::default());
        assert!(!tracker.holds_cli());
    }

    /// Once the CLI sends the level, it replaces the bookends outright: a
    /// missed bookend cannot leave a task stuck running. Ambient tasks are
    /// not counted.
    #[test]
    fn level_replaces_bookends_and_skips_ambient_tasks() {
        let mut tracker = WaitTracker::default();
        feed(
            &mut tracker,
            &[
                system(
                    "task_started",
                    json!({ "tool_use_id": "toolu_never_notified" }),
                ),
                system(
                    "background_tasks_changed",
                    json!({ "tasks": [
                        { "task_id": "b1", "task_type": "local_bash", "description": "until loop" },
                        { "task_id": "m1", "task_type": "monitor_mcp", "description": "watch", "ambient": true },
                    ], "uuid": "u", "session_id": "s" }),
                ),
            ],
        );
        assert_eq!(tracker.waiting_on().background_tasks, 1);

        feed(
            &mut tracker,
            &[system(
                "background_tasks_changed",
                json!({ "tasks": [], "uuid": "u", "session_id": "s" }),
            )],
        );
        assert_eq!(tracker.waiting_on().background_tasks, 0);
    }

    /// `ScheduleWakeup` arms once its call succeeds, clamped to the CLI's
    /// range, and `stop: true` disarms it.
    #[test]
    fn schedule_wakeup_arms_and_stops() {
        let mut tracker = WaitTracker::default();
        feed(
            &mut tracker,
            &[tool_use(
                "toolu_w",
                "ScheduleWakeup",
                json!({ "delaySeconds": 5, "prompt": "go" }),
            )],
        );
        assert_eq!(
            tracker.waiting_on().wakeup_at,
            None,
            "not armed before its result"
        );

        feed(&mut tracker, &[tool_result("toolu_w", "Scheduled.")]);
        assert_eq!(tracker.waiting_on().wakeup_at, Some(T0_UNIX + 60));
        assert!(tracker.waiting_on().is_waiting(T0_UNIX));
        assert!(
            !tracker.waiting_on().is_waiting(T0_UNIX + 60),
            "expires when due"
        );

        feed(
            &mut tracker,
            &[
                tool_use("toolu_s", "ScheduleWakeup", json!({ "stop": true })),
                tool_result("toolu_s", "Loop ended."),
            ],
        );
        assert_eq!(tracker.waiting_on().wakeup_at, None);
    }

    /// A recurring cron holds until `CronDelete`; a failed create holds
    /// nothing.
    #[test]
    fn cron_create_and_delete() {
        let mut tracker = WaitTracker::default();
        feed(
            &mut tracker,
            &[
                tool_use(
                    "toolu_c",
                    "CronCreate",
                    json!({ "cron": "*/5 * * * *", "prompt": "check" }),
                ),
                tool_result(
                    "toolu_c",
                    "Scheduled recurring job abc123 (every 5 minutes). Session-only (not written \
                     to disk, dies when Claude exits). Auto-expires after 7 days. Use CronDelete \
                     to cancel sooner.",
                ),
            ],
        );
        assert_eq!(tracker.waiting_on().crons, 1);
        assert!(tracker.holds_cli());

        feed(
            &mut tracker,
            &[
                tool_use("toolu_d", "CronDelete", json!({ "id": "abc123" })),
                tool_result("toolu_d", "Cancelled abc123."),
            ],
        );
        assert_eq!(tracker.waiting_on().crons, 0);
    }

    /// A one-shot cron is consumed by a wake-up turn no task explains; a
    /// wake-up turn following a task's notification leaves it armed.
    #[test]
    fn one_shot_cron_clears_when_it_fires() {
        let mut tracker = WaitTracker::default();
        feed(
            &mut tracker,
            &[
                tool_use("toolu_c", "CronCreate", json!({ "cron": "30 14 * * *", "recurring": false })),
                tool_result(
                    "toolu_c",
                    "Scheduled one-shot task xyz (at 14:30). Session-only. It will fire once then auto-delete.",
                ),
                turn_result(),
                system("task_notification", json!({ "tool_use_id": "toolu_bg" })),
            ],
        );
        tracker.wakeup_turn_started(T0_UNIX);
        assert_eq!(
            tracker.waiting_on().crons,
            1,
            "the task's wake-up is not the cron"
        );

        feed(&mut tracker, &[turn_result()]);
        tracker.wakeup_turn_started(T0_UNIX);
        assert_eq!(tracker.waiting_on().crons, 0);
    }

    /// Only changes are reported, and losing the CLI reports everything gone.
    #[test]
    fn take_change_reports_each_change_once() {
        let mut tracker = WaitTracker::default();
        assert_eq!(tracker.take_change(), None);
        feed(
            &mut tracker,
            &[system("task_started", json!({ "tool_use_id": "t" }))],
        );
        assert_eq!(tracker.take_change().map(|w| w.background_tasks), Some(1));
        assert_eq!(tracker.take_change(), None);

        tracker.clear();
        assert_eq!(tracker.take_change(), Some(WaitingOn::default()));
    }

    #[test]
    fn parses_cron_results() {
        assert_eq!(
            parse_cron_created("Scheduled recurring job j1 (hourly). Persisted."),
            Some(CronCreated {
                id: "j1".into(),
                recurring: true
            })
        );
        assert_eq!(
            parse_cron_created("Scheduled one-shot task j2 (at noon)."),
            Some(CronCreated {
                id: "j2".into(),
                recurring: false
            })
        );
        assert_eq!(parse_cron_created("Error: bad cron"), None);
    }
}
