//! Remote parity: the inspector state a host publishes for its sessions, and
//! how an observer folds it back in.
//!
//! The host side takes a [`SessionInspector`] apart into an
//! [`InspectorSnapshot`] ([`SessionInspector::snapshot`]) whenever what it
//! holds has moved ([`SnapshotKey`]), at most once every
//! [`SNAPSHOT_INTERVAL`] ([`SnapshotPublisher`]). The observer side puts one
//! back together ([`SessionInspector::restore`]), newest revision only
//! ([`RemoteSnapshot`]). The wire shape and the event live in
//! [`agentium_core::inspector_snapshot`], so the `agentium` CLI reads the
//! same thing.

use super::SessionInspector;
use crate::messages::RateLimitInfo;
use agentium_core::inspector_snapshot::{InspectorSnapshot, UsageSnapshot};
use std::time::{Duration, Instant};

/// Least time between two snapshots of one session.
pub const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(5);

/// What a snapshot was built from: one counter per part of the inspector
/// that changes, so a frame can tell whether anything moved without building
/// one. Copy and allocation-free, because it is read every frame for every
/// local session.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SnapshotKey {
    /// Init and CLI-state reports.
    pub overview: u64,
    pub usage: u64,
    /// The session's own last rate-limit report.
    pub rate_limit: Option<RateLimitInfo>,
    pub background: u64,
    pub subagents: u64,
    /// Temp-dir listings landed.
    pub files: u64,
    /// `git log` reads landed.
    pub commits: u64,
}

/// Whether a session's snapshot should go out this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishDue {
    /// Nothing moved since the last one, or nothing ever happened.
    Idle,
    /// Something moved; publish now.
    Now,
    /// Something moved, but the last went out too recently: try again after
    /// this long.
    After(Duration),
}

/// The host's side of one session's snapshot: what the last one was built
/// from, and when it went out.
#[derive(Debug, Default)]
pub struct SnapshotPublisher {
    /// The key the last published snapshot was built from.
    published: Option<SnapshotKey>,
    /// When it went out.
    published_at: Option<Instant>,
    /// Its `created_at`, so the next is stamped strictly later.
    created_at: u64,
}

impl SnapshotPublisher {
    /// Whether a snapshot built at `key` should go out at `now`.
    ///
    /// A session nothing has happened to since Dave started (a restored,
    /// dormant one) has the default key and never publishes: the snapshot it
    /// published before the restart is still the newest.
    pub fn due(&self, key: SnapshotKey, now: Instant) -> PublishDue {
        let unchanged = match self.published {
            Some(published) => published == key,
            None => key == SnapshotKey::default(),
        };
        if unchanged {
            return PublishDue::Idle;
        }
        match self.published_at {
            Some(at) if now.duration_since(at) < SNAPSHOT_INTERVAL => {
                PublishDue::After(SNAPSHOT_INTERVAL - now.duration_since(at))
            }
            _ => PublishDue::Now,
        }
    }

    /// The `created_at` to stamp the next snapshot with at `now_secs`:
    /// strictly after the last, so a newer revision never ties an older one.
    pub fn next_created_at(&self, now_secs: u64) -> u64 {
        agentium_core::session_events::next_state_created_at(now_secs, self.created_at)
    }

    /// Record that the snapshot built at `key` went out at `now`, stamped
    /// `created_at`.
    pub fn published(&mut self, key: SnapshotKey, now: Instant, created_at: u64) {
        self.published = Some(key);
        self.published_at = Some(now);
        self.created_at = created_at;
    }
}

/// The observer's side of a remote session's snapshot.
#[derive(Debug, Default)]
pub struct RemoteSnapshot {
    /// The `created_at` of the revision folded in, so an older one arriving
    /// late is ignored.
    pub applied_at: u64,
    /// The newest stored revision was looked up, so the session shows the
    /// host's last snapshot before the next one arrives live.
    pub looked_up: bool,
}

impl RemoteSnapshot {
    /// Whether a revision stamped `created_at` is newer than the one folded
    /// in. Marks it folded when it is.
    pub fn accept(&mut self, created_at: u64) -> bool {
        if created_at <= self.applied_at {
            return false;
        }
        self.applied_at = created_at;
        true
    }
}

impl SessionInspector {
    /// What the inspector holds now, as [`SnapshotKey`] counters.
    pub fn snapshot_key(&self) -> SnapshotKey {
        SnapshotKey {
            overview: self.overview_generation,
            usage: self.usage.generation(),
            rate_limit: self.rate_limit,
            background: self.background.generation(),
            subagents: self.subagents.generation(),
            files: self.files.generation(),
            commits: self.outputs.commits.generation(),
        }
    }

    /// The inspector as a snapshot for observers, with `rate_limit` as the
    /// account's windows (newer than any one session's report).
    pub fn snapshot(&self, rate_limit: Option<RateLimitInfo>) -> InspectorSnapshot {
        let usage = &self.usage;
        InspectorSnapshot {
            session_info: self.session_info.clone(),
            cli: self.cli,
            usage: UsageSnapshot {
                current: usage.current(),
                turns: usage.turns().iter().copied().collect(),
                compactions: usage.compactions().iter().copied().collect(),
                turns_recorded: usage.turns_recorded(),
                requests: usage.requests(),
                total_cost: usage.total_cost(),
            },
            rate_limit: rate_limit.or(self.rate_limit),
            background: self.background.entries().to_vec(),
            subagents: self.subagents.all_progress(),
            files: self.files.listing().cloned(),
            commits: self.outputs.commits.list().cloned(),
        }
    }

    /// Replace what the inspector shows with a host's snapshot. For a remote
    /// session, whose backend streams nothing here.
    pub fn restore(&mut self, snapshot: InspectorSnapshot) {
        let InspectorSnapshot {
            session_info,
            cli,
            usage,
            rate_limit,
            background,
            subagents,
            files,
            commits,
        } = snapshot;
        self.session_info = session_info;
        self.cli = cli;
        self.overview_generation += 1;
        self.usage.restore(usage);
        self.rate_limit = rate_limit;
        self.background.set(background);
        self.subagents.replace(subagents);
        self.files.set_remote(files);
        self.outputs.commits.set_remote(commits);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{
        BackgroundEntry, BackgroundTask, BackgroundWork, CliState, DaveApiResponse,
        SubagentProgress, UsageInfo,
    };

    fn busy_inspector() -> SessionInspector {
        let mut inspector = SessionInspector::default();
        inspector.apply(&DaveApiResponse::CliState(CliState {
            connected: true,
            reap_at: Some(9),
        }));
        inspector.apply(&DaveApiResponse::UsageUpdate(UsageInfo {
            input_tokens: 10,
            cache_read_input_tokens: 5000,
            output_tokens: 30,
            ..Default::default()
        }));
        inspector.apply(&DaveApiResponse::QueryComplete(UsageInfo {
            cost_usd: Some(0.25),
            num_turns: 2,
            ..Default::default()
        }));
        inspector.apply(&DaveApiResponse::BackgroundWork(vec![BackgroundEntry {
            id: "b1".into(),
            started_at: 3,
            work: BackgroundWork::Task(BackgroundTask {
                description: "cargo test".into(),
                ..Default::default()
            }),
        }]));
        inspector.apply(&DaveApiResponse::SubagentProgress(SubagentProgress {
            task_id: "toolu_1".into(),
            last_tool_name: Some("Grep".into()),
            ..Default::default()
        }));
        inspector
    }

    /// A snapshot restored on an observer reads back as the host had it.
    #[test]
    fn restore_reproduces_the_hosts_snapshot() {
        let host = busy_inspector();
        let snapshot = host.snapshot(None);

        let mut observer = SessionInspector::default();
        observer.restore(snapshot.clone());

        assert_eq!(observer.snapshot(None), snapshot);
        assert_eq!(observer.usage.turns_recorded(), 1);
        assert_eq!(observer.usage.total_cost(), Some(0.25));
        assert_eq!(observer.usage.sparkline().values.len(), 1);
        assert_eq!(observer.background.entries()[0].id, "b1");
        assert_eq!(
            observer
                .subagents
                .progress("toolu_1")
                .and_then(|p| p.last_tool_name.as_deref()),
            Some("Grep")
        );
    }

    /// A burst of changes inside one interval publishes once, then once
    /// more for whatever moved after it, and an unchanged inspector never.
    #[test]
    fn a_burst_of_changes_publishes_once_per_interval() {
        let mut inspector = SessionInspector::default();
        let mut publisher = SnapshotPublisher::default();
        let start = Instant::now();
        assert_eq!(
            publisher.due(inspector.snapshot_key(), start),
            PublishDue::Idle
        );

        let mut published = 0;
        for i in 0..20u64 {
            inspector.apply(&DaveApiResponse::UsageUpdate(UsageInfo {
                input_tokens: i,
                ..Default::default()
            }));
            let now = start + Duration::from_millis(100 * i);
            let key = inspector.snapshot_key();
            if publisher.due(key, now) == PublishDue::Now {
                publisher.published(key, now, publisher.next_created_at(100));
                published += 1;
            }
        }
        assert_eq!(published, 1, "two seconds of updates is one snapshot");

        let later = start + Duration::from_millis(1900);
        assert!(matches!(
            publisher.due(inspector.snapshot_key(), later),
            PublishDue::After(_)
        ));
        let after_interval = start + SNAPSHOT_INTERVAL;
        assert_eq!(
            publisher.due(inspector.snapshot_key(), after_interval),
            PublishDue::Now
        );
        let key = inspector.snapshot_key();
        publisher.published(key, after_interval, publisher.next_created_at(100));
        assert_eq!(publisher.created_at, 101, "same second is stamped later");
        assert_eq!(
            publisher.due(key, after_interval + SNAPSHOT_INTERVAL),
            PublishDue::Idle
        );
    }

    /// An observer folds only revisions newer than the one it has.
    #[test]
    fn remote_snapshot_accepts_only_newer_revisions() {
        let mut remote = RemoteSnapshot::default();
        assert!(remote.accept(10));
        assert!(!remote.accept(10));
        assert!(!remote.accept(9));
        assert!(remote.accept(11));
    }
}
