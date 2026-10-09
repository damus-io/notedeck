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
//! Beside the counts ([`WaitingOn`]) it keeps what each one is
//! ([`BackgroundEntry`]): a task's type, description and progress from the
//! CLI's task messages, and its command and output file from the `Bash` or
//! `Monitor` call that launched it, which the task messages name only by
//! `tool_use_id`; a wake-up's reason and prompt; a cron's expression.
//!
//! Not visible here: a wake-up the CLI arms on its own (the `/loop` keepalive
//! fallback when the model does not reschedule), and a durable cron that
//! outlives the CLI. Both resume the session without a tool call in the
//! stream.

use super::claude::parse_user_content_blocks;
use crate::messages::{
    BackgroundEntry, BackgroundTask, BackgroundTaskKind, BackgroundWork, TaskUsage, WaitingOn,
};
use claude_agent_sdk_rs::{
    ContentBlock, Message as ClaudeMessage, ToolResultContent, ToolUseBlock,
};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::time::Instant;

/// The CLI clamps a `ScheduleWakeup` delay to this range (seconds).
const WAKEUP_DELAY_RANGE: (u64, u64) = (60, 3600);

/// The [`BackgroundEntry::id`] of the armed wake-up; there is at most one.
const WAKEUP_ENTRY_ID: &str = "wakeup";

/// A tool call that changes what is scheduled, held from its `tool_use` until
/// its result says whether it took effect.
enum Scheduling {
    /// `ScheduleWakeup` arming a wake-up `delay_secs` from its result.
    Wakeup {
        delay_secs: u64,
        reason: String,
        prompt: String,
    },
    /// `ScheduleWakeup { stop: true }`: the loop ends, nothing is armed.
    StopWakeup,
    /// `CronCreate`; the job id, kind and durability are in its result text.
    CronCreate { expr: String, prompt: String },
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
                    reason: input_str(input, "reason").unwrap_or_default().to_string(),
                    prompt: input_str(input, "prompt").unwrap_or_default().to_string(),
                })
            }
            "CronCreate" => Some(Self::CronCreate {
                expr: input_str(input, "cron").unwrap_or_default().to_string(),
                prompt: input_str(input, "prompt").unwrap_or_default().to_string(),
            }),
            "CronDelete" => {
                let id = input_str(input, "id")?;
                Some(Self::CronDelete { id: id.to_string() })
            }
            _ => None,
        }
    }
}

/// An armed `ScheduleWakeup`, in both clocks: Unix seconds for the wire, an
/// [`Instant`] for the idle reaper.
struct Wakeup {
    at_unix: u64,
    at: Instant,
    armed_at_unix: u64,
    reason: String,
    prompt: String,
}

/// A `CronCreate` job, until it fires (one-shot) or is deleted.
struct CronJob {
    expr: String,
    prompt: String,
    recurring: bool,
    durable: bool,
    created_at: u64,
}

/// A background task's detail, gathered from the CLI's task messages. Keyed
/// by task id (or, from a CLI whose `task_started` carries none, by the
/// launching `tool_use_id`).
struct TaskRecord {
    /// The tool call that launched it, which [`WaitTracker::launches`] may
    /// hold the command for.
    tool_use_id: Option<String>,
    kind: BackgroundTaskKind,
    description: String,
    started_at: u64,
    usage: Option<TaskUsage>,
    last_tool_name: Option<String>,
    summary: Option<String>,
}

impl TaskRecord {
    fn new(started_at: u64) -> Self {
        Self {
            tool_use_id: None,
            kind: BackgroundTaskKind::Other,
            description: String::new(),
            started_at,
            usage: None,
            last_tool_name: None,
            summary: None,
        }
    }

    /// Take the type and description a task message carries, keeping what
    /// is already known where it carries none.
    fn update(&mut self, data: &serde_json::Value) {
        if let Some(task_type) = input_str(data, "task_type") {
            self.kind = BackgroundTaskKind::from_task_type(task_type);
        }
        if let Some(description) = input_str(data, "description").filter(|d| !d.is_empty()) {
            if self.description != description {
                self.description = description.to_string();
            }
        }
        if self.tool_use_id.is_none() {
            self.tool_use_id = tool_use_id(data).map(str::to_string);
        }
    }
}

/// A `Bash` or `Monitor` call that may start a background task, held for
/// the command its task's entry shows. The CLI's task messages don't carry
/// the command, only the launching `tool_use_id`.
struct Launch {
    /// The Monitor tool launched it: the CLI reports a command Monitor as a
    /// `local_bash` task.
    monitor: bool,
    command: Option<String>,
    /// Where the output goes, read off the tool's result.
    output_file: Option<String>,
}

/// Everything outstanding that will resume the session. See the module docs.
#[derive(Default)]
pub(crate) struct WaitTracker {
    /// The live background tasks as of the CLI's last
    /// `background_tasks_changed`, by task id, in the CLI's order. `None`
    /// until the CLI sends one.
    level_tasks: Option<Vec<String>>,
    /// Background tasks between `task_started` and `task_notification`, by
    /// launching `tool_use_id`. Only consulted while `level_tasks` is `None`.
    edge_tasks: HashSet<String>,
    /// What is known about each live task. See [`TaskRecord`].
    tasks: HashMap<String, TaskRecord>,
    /// `Bash` and `Monitor` calls that started, or may yet start, a
    /// background task, by `tool_use_id`.
    launches: HashMap<String, Launch>,
    /// Scheduling tool calls whose results have not arrived, by `tool_use_id`.
    scheduling: HashMap<String, Scheduling>,
    wakeup: Option<Wakeup>,
    /// Cron jobs by id.
    crons: HashMap<String, CronJob>,
    /// A background task ended since the last turn did, so the next wake-up
    /// turn is that task's notification and not a cron firing.
    task_ended_since_turn: bool,
    /// What was last reported, so only changes are sent.
    reported: WaitingOn,
    /// The entries last reported, so only changes are sent.
    reported_entries: Vec<BackgroundEntry>,
    /// Something [`Self::entries`] reads changed since the last
    /// [`Self::take_entries_change`].
    entries_dirty: bool,
}

impl WaitTracker {
    /// Account for one message from the CLI. `now` / `now_unix` are the same
    /// moment in both clocks.
    pub(crate) fn on_message(&mut self, now: Instant, now_unix: u64, message: &ClaudeMessage) {
        match message {
            ClaudeMessage::System(system_msg) => {
                self.on_system(&system_msg.subtype, &system_msg.data, now_unix)
            }
            ClaudeMessage::Assistant(assistant_msg) => {
                for block in &assistant_msg.message.content {
                    if let ContentBlock::ToolUse(ToolUseBlock { id, name, input }) = block {
                        if let Some(scheduling) = Scheduling::from_tool_use(name, input) {
                            self.scheduling.insert(id.clone(), scheduling);
                        }
                        if let Some(launch) = Launch::from_tool_use(name, input) {
                            self.launches.insert(id.clone(), launch);
                        }
                    }
                }
            }
            ClaudeMessage::User(user_msg) => {
                for block in parse_user_content_blocks(user_msg) {
                    let ContentBlock::ToolResult(result) = block else {
                        continue;
                    };
                    let text = result_text(&result.content);
                    self.launch_result(&result.tool_use_id, text);
                    let Some(scheduling) = self.scheduling.remove(&result.tool_use_id) else {
                        continue;
                    };
                    if result.is_error == Some(true) {
                        continue;
                    }
                    self.apply(scheduling, text, now, now_unix);
                }
            }
            ClaudeMessage::Result(_) => self.task_ended_since_turn = false,
            _ => {}
        }
    }

    /// A background-task system message.
    fn on_system(&mut self, subtype: &str, data: &serde_json::Value, now_unix: u64) {
        match subtype {
            "background_tasks_changed" => {
                let Some(tasks) = data.get("tasks").and_then(serde_json::Value::as_array) else {
                    return;
                };
                // An ambient task (a watcher nobody asked for) is not activity
                // and wakes nothing on the session's behalf.
                let live: Vec<&serde_json::Value> = tasks
                    .iter()
                    .filter(|t| t.get("ambient").and_then(serde_json::Value::as_bool) != Some(true))
                    .filter(|t| input_str(t, "task_id").is_some())
                    .collect();
                let ids: Vec<String> = live
                    .iter()
                    .filter_map(|t| input_str(t, "task_id"))
                    .map(str::to_string)
                    .collect();
                if let Some(previous) = &self.level_tasks {
                    if previous.iter().any(|id| !ids.contains(id)) {
                        self.task_ended_since_turn = true;
                    }
                }
                for (task, id) in live.iter().zip(&ids) {
                    self.tasks
                        .entry(id.clone())
                        .or_insert_with(|| TaskRecord::new(now_unix))
                        .update(task);
                }
                self.forget_tasks_where(|id, _| !ids.contains(id));
                self.level_tasks = Some(ids);
                self.entries_dirty = true;
            }
            "task_started" => {
                if data.get("ambient").and_then(serde_json::Value::as_bool) != Some(true) {
                    if let Some(key) = task_key(data) {
                        self.tasks
                            .entry(key.to_string())
                            .or_insert_with(|| TaskRecord::new(now_unix))
                            .update(data);
                    }
                }
                if let Some(id) = tool_use_id(data) {
                    self.edge_tasks.insert(id.to_string());
                }
                self.entries_dirty = true;
            }
            "task_progress" => {
                let Some(record) = task_key(data).and_then(|key| self.tasks.get_mut(key)) else {
                    return;
                };
                record.update(data);
                if let Some(usage) = data.get("usage") {
                    let field = |name| usage.get(name).and_then(serde_json::Value::as_u64);
                    record.usage = Some(TaskUsage {
                        total_tokens: field("total_tokens").unwrap_or(0),
                        tool_uses: field("tool_uses").unwrap_or(0),
                        duration_ms: field("duration_ms").unwrap_or(0),
                    });
                }
                if let Some(tool) = input_str(data, "last_tool_name") {
                    record.last_tool_name = Some(tool.to_string());
                }
                if let Some(summary) = input_str(data, "summary").filter(|s| !s.is_empty()) {
                    record.summary = Some(summary.to_string());
                }
                self.entries_dirty = true;
            }
            "task_notification" => {
                self.task_ended_since_turn = true;
                if let Some(id) = tool_use_id(data) {
                    self.edge_tasks.remove(id);
                }
                // Once the CLI sends the level, its next one drops the task.
                if self.level_tasks.is_none() {
                    if let Some(key) = task_key(data).map(str::to_string) {
                        self.forget_tasks_where(|id, _| *id == key);
                    }
                }
                self.entries_dirty = true;
            }
            _ => {}
        }
    }

    /// Drop the task records `gone` picks, and the launches behind them.
    fn forget_tasks_where(&mut self, mut gone: impl FnMut(&String, &TaskRecord) -> bool) {
        let launches = &mut self.launches;
        self.tasks.retain(|id, record| {
            if !gone(id, record) {
                return true;
            }
            if let Some(tool_use_id) = &record.tool_use_id {
                launches.remove(tool_use_id);
            }
            false
        });
    }

    /// A tool result arrived. A launch whose call didn't go to the
    /// background is forgotten; one that did keeps its output path.
    fn launch_result(&mut self, tool_use_id: &str, text: Option<&str>) {
        let Some(launch) = self.launches.get_mut(tool_use_id) else {
            return;
        };
        let output_file = text.and_then(parse_output_file);
        let backgrounded = output_file.is_some()
            || text.is_some_and(|t| t.starts_with("Monitor started"))
            || self
                .tasks
                .values()
                .any(|r| r.tool_use_id.as_deref() == Some(tool_use_id));
        if !backgrounded {
            self.launches.remove(tool_use_id);
            return;
        }
        if output_file.is_some() {
            launch.output_file = output_file;
            self.entries_dirty = true;
        }
    }

    /// A scheduling tool call succeeded.
    fn apply(&mut self, scheduling: Scheduling, result: Option<&str>, now: Instant, now_unix: u64) {
        self.entries_dirty = true;
        match scheduling {
            Scheduling::Wakeup {
                delay_secs,
                reason,
                prompt,
            } => {
                self.wakeup = Some(Wakeup {
                    at_unix: now_unix + delay_secs,
                    at: now + Duration::from_secs(delay_secs),
                    armed_at_unix: now_unix,
                    reason,
                    prompt,
                });
            }
            Scheduling::StopWakeup => self.wakeup = None,
            Scheduling::CronCreate { expr, prompt } => match result.and_then(parse_cron_created) {
                Some(CronCreated {
                    id,
                    recurring,
                    durable,
                }) => {
                    self.crons.insert(
                        id,
                        CronJob {
                            expr,
                            prompt,
                            recurring,
                            durable,
                            created_at: now_unix,
                        },
                    );
                }
                None => tracing::debug!("CronCreate result without a job id: {result:?}"),
            },
            Scheduling::CronDelete { id } => {
                self.crons.remove(&id);
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
        self.entries_dirty = true;
        // The CLI fires on its own clock; allow it to run a little early.
        const DUE_SLACK_SECS: u64 = 60;
        match &self.wakeup {
            Some(wakeup) if wakeup.at_unix <= now_unix + DUE_SLACK_SECS => self.wakeup = None,
            _ => self.crons.retain(|_, job| job.recurring),
        }
    }

    /// The CLI is gone, and everything it would have run with it.
    pub(crate) fn clear(&mut self) {
        let reported = self.reported;
        let reported_entries = std::mem::take(&mut self.reported_entries);
        *self = Self {
            reported,
            reported_entries,
            entries_dirty: true,
            ..Self::default()
        };
    }

    /// Live background tasks.
    fn background_tasks(&self) -> usize {
        match &self.level_tasks {
            Some(level) => level.len(),
            None => self.edge_tasks.len(),
        }
    }

    /// Whether something outstanding lives in the CLI process and would die
    /// with it: a background task or a cron. Its due wake-up is separate
    /// (see [`wakeup_at`](Self::wakeup_at)).
    pub(crate) fn holds_cli(&self) -> bool {
        self.background_tasks() > 0 || !self.crons.is_empty()
    }

    /// When the armed wake-up is due, if one is.
    pub(crate) fn wakeup_at(&self) -> Option<Instant> {
        self.wakeup.as_ref().map(|w| w.at)
    }

    /// The current state, as the UI sees it.
    pub(crate) fn waiting_on(&self) -> WaitingOn {
        WaitingOn {
            background_tasks: u32::try_from(self.background_tasks()).unwrap_or(u32::MAX),
            wakeup_at: self.wakeup.as_ref().map(|w| w.at_unix),
            crons: u32::try_from(self.crons.len()).unwrap_or(u32::MAX),
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

    /// Every live task, the wake-up and each cron, newest first: the detail
    /// behind [`Self::waiting_on`]'s counts.
    pub(crate) fn entries(&self) -> Vec<BackgroundEntry> {
        let live_tasks: Vec<(&String, &TaskRecord)> = match &self.level_tasks {
            Some(level) => level
                .iter()
                .filter_map(|id| self.tasks.get_key_value(id))
                .collect(),
            None => self
                .tasks
                .iter()
                .filter(|(_, r)| {
                    r.tool_use_id
                        .as_ref()
                        .is_some_and(|t| self.edge_tasks.contains(t))
                })
                .collect(),
        };
        let mut entries: Vec<BackgroundEntry> = live_tasks
            .into_iter()
            .map(|(id, record)| self.task_entry(id, record))
            .collect();
        if let Some(wakeup) = &self.wakeup {
            entries.push(BackgroundEntry {
                id: WAKEUP_ENTRY_ID.to_string(),
                started_at: wakeup.armed_at_unix,
                work: BackgroundWork::Wakeup {
                    due_at: wakeup.at_unix,
                    reason: wakeup.reason.clone(),
                    prompt: wakeup.prompt.clone(),
                },
            });
        }
        entries.extend(self.crons.iter().map(|(id, job)| BackgroundEntry {
            id: id.clone(),
            started_at: job.created_at,
            work: BackgroundWork::Cron {
                expr: job.expr.clone(),
                prompt: job.prompt.clone(),
                recurring: job.recurring,
                durable: job.durable,
            },
        }));
        entries.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        entries
    }

    /// One task's entry, with its launching call's command if it was held.
    fn task_entry(&self, id: &str, record: &TaskRecord) -> BackgroundEntry {
        let launch = record
            .tool_use_id
            .as_ref()
            .and_then(|t| self.launches.get(t));
        let kind = match launch {
            Some(Launch { monitor: true, .. }) => BackgroundTaskKind::Monitor,
            _ => record.kind,
        };
        BackgroundEntry {
            id: id.to_string(),
            started_at: record.started_at,
            work: BackgroundWork::Task(BackgroundTask {
                kind,
                description: record.description.clone(),
                command: launch.and_then(|l| l.command.clone()),
                output_file: launch.and_then(|l| l.output_file.clone()),
                usage: record.usage,
                last_tool_name: record.last_tool_name.clone(),
                summary: record.summary.clone(),
            }),
        }
    }

    /// The entries, if they changed since last reported. Builds nothing
    /// unless something behind them changed, so it is cheap to call per CLI
    /// message.
    pub(crate) fn take_entries_change(&mut self) -> Option<Vec<BackgroundEntry>> {
        if !std::mem::take(&mut self.entries_dirty) {
            return None;
        }
        let entries = self.entries();
        if entries == self.reported_entries {
            return None;
        }
        self.reported_entries = entries.clone();
        Some(entries)
    }
}

impl Launch {
    /// The launch a tool call may be: a `Bash` command or a `Monitor`.
    fn from_tool_use(name: &str, input: &serde_json::Value) -> Option<Self> {
        let monitor = match name {
            "Bash" => false,
            "Monitor" => true,
            _ => return None,
        };
        // A socket Monitor watches a URL rather than running a command.
        let command = input_str(input, "command")
            .or_else(|| input_str(input, "url"))
            .map(str::to_string);
        Some(Self {
            monitor,
            command,
            output_file: None,
        })
    }
}

/// A string field of a tool input or message.
fn input_str<'a>(value: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(serde_json::Value::as_str)
}

/// The launching `tool_use_id` a task message carries.
fn tool_use_id(data: &serde_json::Value) -> Option<&str> {
    input_str(data, "tool_use_id")
}

/// The key a task message's record is kept under: its task id, or the
/// launching `tool_use_id` from a CLI that sends none.
fn task_key(data: &serde_json::Value) -> Option<&str> {
    input_str(data, "task_id").or_else(|| tool_use_id(data))
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

/// The output path a backgrounded Bash call's result names: "… Output is
/// being written to: /tmp/…/tasks/b1.output."
fn parse_output_file(text: &str) -> Option<String> {
    const MARKER: &str = "Output is being written to: ";
    let rest = &text[text.find(MARKER)? + MARKER.len()..];
    let path = rest.split_whitespace().next()?.trim_end_matches('.');
    (!path.is_empty()).then(|| path.to_string())
}

/// A cron job `CronCreate` scheduled.
#[derive(Debug, PartialEq, Eq)]
struct CronCreated {
    id: String,
    recurring: bool,
    /// Written to `.claude/scheduled_tasks.json`, so it outlives the CLI.
    durable: bool,
}

/// Read the job out of a `CronCreate` result: "Scheduled recurring job <id>
/// (...)" or "Scheduled one-shot task <id> (...)", then "Persisted to …" for
/// a durable job.
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
        durable: text.contains("Persisted to "),
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

    /// The one entry, or a panic.
    fn only_entry(tracker: &WaitTracker) -> BackgroundEntry {
        let entries = tracker.entries();
        assert_eq!(entries.len(), 1, "{entries:?}");
        entries.into_iter().next().unwrap()
    }

    /// The task an entry is, or a panic.
    fn task_of(entry: &BackgroundEntry) -> &BackgroundTask {
        let BackgroundWork::Task(task) = &entry.work else {
            panic!("not a task: {entry:?}");
        };
        task
    }

    /// A background Bash command becomes one entry: its command and output
    /// file from the launching call, its description from the level, and
    /// its usage and last tool from `task_progress`. Its end removes it.
    #[test]
    fn a_background_task_is_one_entry_with_its_command_and_progress() {
        let mut tracker = WaitTracker::default();
        feed(
            &mut tracker,
            &[
                tool_use(
                    "toolu_bg",
                    "Bash",
                    json!({ "command": "cargo test -p notedeck_dave", "run_in_background": true }),
                ),
                system(
                    "task_started",
                    json!({ "task_id": "b1", "tool_use_id": "toolu_bg", "task_type": "local_bash",
                            "description": "run the tests" }),
                ),
                tool_result(
                    "toolu_bg",
                    "Command running in background with ID: b1. Output is being written to: \
                     /tmp/claude-1000/x/tasks/b1.output.",
                ),
                system(
                    "background_tasks_changed",
                    json!({ "tasks": [
                        { "task_id": "b1", "task_type": "local_bash", "description": "run the tests" },
                    ] }),
                ),
                system(
                    "task_progress",
                    json!({ "task_id": "b1", "tool_use_id": "toolu_bg", "description": "run the tests",
                            "usage": { "total_tokens": 1200, "tool_uses": 3, "duration_ms": 4500 },
                            "last_tool_name": "Read", "summary": "compiling" }),
                ),
            ],
        );

        let entry = only_entry(&tracker);
        let task = task_of(&entry);
        assert_eq!(entry.id, "b1");
        assert_eq!(entry.started_at, T0_UNIX);
        assert_eq!(task.kind, BackgroundTaskKind::Bash);
        assert_eq!(task.description, "run the tests");
        assert_eq!(task.command.as_deref(), Some("cargo test -p notedeck_dave"));
        assert_eq!(
            task.output_file.as_deref(),
            Some("/tmp/claude-1000/x/tasks/b1.output")
        );
        assert_eq!(
            task.usage,
            Some(TaskUsage {
                total_tokens: 1200,
                tool_uses: 3,
                duration_ms: 4500
            })
        );
        assert_eq!(task.last_tool_name.as_deref(), Some("Read"));
        assert_eq!(task.summary.as_deref(), Some("compiling"));

        feed(
            &mut tracker,
            &[
                system(
                    "task_notification",
                    json!({ "task_id": "b1", "tool_use_id": "toolu_bg", "status": "completed" }),
                ),
                system("background_tasks_changed", json!({ "tasks": [] })),
            ],
        );
        assert!(tracker.entries().is_empty());
        assert!(tracker.launches.is_empty(), "the launch goes with its task");
    }

    /// A command the Monitor tool launched is a Monitor, though the CLI
    /// reports it as `local_bash`. A foreground Bash call holds nothing.
    #[test]
    fn a_monitor_launch_marks_its_task_and_a_foreground_bash_is_forgotten() {
        let mut tracker = WaitTracker::default();
        feed(
            &mut tracker,
            &[
                tool_use("toolu_ls", "Bash", json!({ "command": "ls" })),
                tool_result("toolu_ls", "Cargo.toml"),
                tool_use(
                    "toolu_m",
                    "Monitor",
                    json!({ "command": "tail -f log", "description": "watch the log" }),
                ),
                system(
                    "task_started",
                    json!({ "task_id": "m1", "tool_use_id": "toolu_m", "task_type": "local_bash",
                            "description": "watch the log" }),
                ),
                tool_result("toolu_m", "Monitor started (task m1, timeout 300000ms)."),
            ],
        );
        assert!(!tracker.launches.contains_key("toolu_ls"));
        let entry = only_entry(&tracker);
        let task = task_of(&entry);
        assert_eq!(task.kind, BackgroundTaskKind::Monitor);
        assert_eq!(task.command.as_deref(), Some("tail -f log"));
    }

    /// A wake-up's entry carries its reason, prompt and due time; a cron's
    /// its expression and durability. Newest first.
    #[test]
    fn wakeup_and_cron_entries_carry_their_detail() {
        let mut tracker = WaitTracker::default();
        let now = Instant::now();
        let messages = [
            tool_use(
                "toolu_c",
                "CronCreate",
                json!({ "cron": "*/5 * * * *", "prompt": "check CI", "durable": true }),
            ),
            tool_result(
                "toolu_c",
                "Scheduled recurring job abc (every 5 minutes). Persisted to \
                 .claude/scheduled_tasks.json. Auto-expires after 7 days.",
            ),
        ];
        for message in &messages {
            tracker.on_message(now, T0_UNIX, message);
        }
        let messages = [
            tool_use(
                "toolu_w",
                "ScheduleWakeup",
                json!({ "delaySeconds": 270, "reason": "watching CI", "prompt": "/loop check" }),
            ),
            tool_result("toolu_w", "Scheduled."),
        ];
        for message in &messages {
            tracker.on_message(now, T0_UNIX + 10, message);
        }

        let entries = tracker.entries();
        assert_eq!(
            entries,
            [
                BackgroundEntry {
                    id: "wakeup".into(),
                    started_at: T0_UNIX + 10,
                    work: BackgroundWork::Wakeup {
                        due_at: T0_UNIX + 280,
                        reason: "watching CI".into(),
                        prompt: "/loop check".into(),
                    },
                },
                BackgroundEntry {
                    id: "abc".into(),
                    started_at: T0_UNIX,
                    work: BackgroundWork::Cron {
                        expr: "*/5 * * * *".into(),
                        prompt: "check CI".into(),
                        recurring: true,
                        durable: true,
                    },
                },
            ]
        );
    }

    /// Entries are reported once per change, and not rebuilt for messages
    /// that touch nothing behind them.
    #[test]
    fn take_entries_change_reports_each_change_once() {
        let mut tracker = WaitTracker::default();
        assert_eq!(tracker.take_entries_change(), None);
        feed(
            &mut tracker,
            &[system(
                "task_started",
                json!({ "task_id": "b1", "tool_use_id": "t", "task_type": "local_bash" }),
            )],
        );
        assert_eq!(tracker.take_entries_change().map(|e| e.len()), Some(1));
        assert!(!tracker.entries_dirty);
        feed(&mut tracker, &[turn_result()]);
        assert_eq!(tracker.take_entries_change(), None);

        tracker.clear();
        assert_eq!(tracker.take_entries_change(), Some(Vec::new()));
    }

    #[test]
    fn parses_output_files() {
        assert_eq!(
            parse_output_file(
                "Command running in background with ID: b1. Output is being written to: /tmp/t/b1.output."
            )
            .as_deref(),
            Some("/tmp/t/b1.output")
        );
        assert_eq!(parse_output_file("exit 0"), None);
    }

    #[test]
    fn parses_cron_results() {
        assert_eq!(
            parse_cron_created(
                "Scheduled recurring job j1 (hourly). Persisted to .claude/scheduled_tasks.json."
            ),
            Some(CronCreated {
                id: "j1".into(),
                recurring: true,
                durable: true,
            })
        );
        assert_eq!(
            parse_cron_created("Scheduled one-shot task j2 (at noon)."),
            Some(CronCreated {
                id: "j2".into(),
                recurring: false,
                durable: false,
            })
        );
        assert_eq!(parse_cron_created("Error: bad cron"), None);
    }
}
