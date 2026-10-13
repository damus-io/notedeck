//! Remote parity: the inspector state a host publishes for its sessions, and
//! how an observer folds it back in.
//!
//! The host side takes a [`SessionInspector`] apart into an
//! [`InspectorSnapshot`] ([`SessionInspector::snapshot`]) whenever what it
//! holds has moved ([`SnapshotKey`]), at most once every
//! [`SNAPSHOT_INTERVAL`] while the session is settled and once every
//! [`STREAMING_SNAPSHOT_INTERVAL`] while a turn runs ([`SnapshotPublisher`]). The observer side puts one
//! back together ([`SessionInspector::restore`]), newest revision only
//! ([`RemoteSnapshot`]). The wire shape and the event live in
//! [`agentium_core::inspector_snapshot`], so the `agentium` CLI reads the
//! same thing.

use super::SessionInspector;
use crate::git_status::GitStatusCache;
use crate::messages::RateLimitInfo;
use agentium_core::inspector_snapshot::{GitStatus, InspectorSnapshot, UsageSnapshot};
use std::time::{Duration, Instant};

/// Least time between two snapshots of a session with no turn running, so
/// the one that goes out when a turn ends follows it closely.
pub const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(5);

/// Least time between two snapshots of a session while a turn runs. Every
/// snapshot is kept as its own version in ndb (and again as its PNS
/// envelope), and a streaming turn moves something every few seconds, so a
/// remote reader gets a coarse view of the turn and the settled one when it
/// ends.
pub const STREAMING_SNAPSHOT_INTERVAL: Duration = Duration::from_secs(60);

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
    /// Working-tree reads that changed it, counted only while the CLI runs
    /// (zero otherwise): a dormant session's tree moves when another session
    /// in the same cwd edits it, and that must not republish every session
    /// there.
    pub git: u64,
}

impl SnapshotKey {
    /// Whether a snapshot built at `self` shows a reader something one built
    /// at `published` didn't. While a turn runs (`turn_running`) the usage
    /// counter alone doesn't count: it moves on every token tick, and the
    /// snapshot that goes out when the turn ends carries the turn's usage.
    fn moved_from(&self, published: &SnapshotKey, turn_running: bool) -> bool {
        if !turn_running {
            return self != published;
        }
        SnapshotKey {
            usage: published.usage,
            ..*self
        } != *published
    }
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
    /// Whether a snapshot built at `key` should go out at `now`, with
    /// `turn_running` saying whether the session is in a turn (see
    /// [`STREAMING_SNAPSHOT_INTERVAL`]).
    ///
    /// A session nothing has happened to since Dave started (a restored,
    /// dormant one) has the default key and never publishes: the snapshot it
    /// published before the restart is still the newest.
    pub fn due(&self, key: SnapshotKey, turn_running: bool, now: Instant) -> PublishDue {
        let published = self.published.unwrap_or_default();
        if !key.moved_from(&published, turn_running) {
            return PublishDue::Idle;
        }
        let interval = if turn_running {
            STREAMING_SNAPSHOT_INTERVAL
        } else {
            SNAPSHOT_INTERVAL
        };
        match self.published_at {
            Some(at) if now.duration_since(at) < interval => {
                PublishDue::After(interval - now.duration_since(at))
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
    /// What the inspector holds now, with the session's working tree `git`,
    /// as [`SnapshotKey`] counters.
    pub fn snapshot_key(&self, git: &GitStatusCache) -> SnapshotKey {
        let running = self.cli.is_some_and(|cli| cli.connected);
        SnapshotKey {
            overview: self.overview_generation,
            usage: self.usage.generation(),
            rate_limit: self.rate_limit,
            background: self.background.generation(),
            subagents: self.subagents.generation(),
            files: self.files.generation(),
            commits: self.outputs.commits.generation(),
            git: if running { git.generation() } else { 0 },
        }
    }

    /// The inspector as a snapshot for observers, with `rate_limit` as the
    /// account's windows (newer than any one session's report) and `git` as
    /// the session's working tree.
    pub fn snapshot(
        &self,
        rate_limit: Option<RateLimitInfo>,
        git: &GitStatusCache,
    ) -> InspectorSnapshot {
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
            git: git.current().cloned(),
        }
    }

    /// Replace what the inspector shows with a host's snapshot. For a remote
    /// session, whose backend streams nothing here.
    ///
    /// Returns the snapshot's working tree, which is the session's
    /// [`GitStatusCache`]'s to hold rather than the inspector's.
    pub fn restore(&mut self, snapshot: InspectorSnapshot) -> Option<GitStatus> {
        let InspectorSnapshot {
            session_info,
            cli,
            usage,
            rate_limit,
            background,
            subagents,
            files,
            commits,
            git,
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
        git
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{
        BackgroundEntry, BackgroundTask, BackgroundWork, CliState, DaveApiResponse,
        SubagentProgress, TaskUsage, UsageInfo,
    };

    /// `inspector`'s key beside a working tree that never landed a read.
    fn key_of(inspector: &SessionInspector) -> SnapshotKey {
        inspector.snapshot_key(&GitStatusCache::new("/tmp/project".into()))
    }

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
        let mut host_git = GitStatusCache::new("/tmp/project".into());
        host_git.set_lines(142, 37);
        let snapshot = host.snapshot(None, &host_git);
        assert!(snapshot.git.is_some());

        let mut observer = SessionInspector::default();
        let mut observer_git = GitStatusCache::new("/tmp/project".into());
        observer_git.set_remote(observer.restore(snapshot.clone()));

        assert_eq!(observer.snapshot(None, &observer_git), snapshot);
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

    /// With no turn running, a burst of changes inside one interval publishes
    /// once, then once more for whatever moved after it, and an unchanged
    /// inspector never.
    #[test]
    fn a_burst_of_changes_publishes_once_per_interval() {
        let mut inspector = SessionInspector::default();
        let git = GitStatusCache::new("/tmp/project".into());
        let mut publisher = SnapshotPublisher::default();
        let start = Instant::now();
        assert_eq!(
            publisher.due(key_of(&inspector), false, start),
            PublishDue::Idle
        );

        let mut published = 0;
        for i in 0..20u64 {
            inspector.apply(&DaveApiResponse::UsageUpdate(UsageInfo {
                input_tokens: i,
                ..Default::default()
            }));
            let now = start + Duration::from_millis(100 * i);
            let key = key_of(&inspector);
            if publisher.due(key, false, now) == PublishDue::Now {
                publisher.published(key, now, publisher.next_created_at(100));
                published += 1;
            }
        }
        assert_eq!(published, 1, "two seconds of updates is one snapshot");

        let later = start + Duration::from_millis(1900);
        assert!(matches!(
            publisher.due(key_of(&inspector), false, later),
            PublishDue::After(_)
        ));
        let after_interval = start + SNAPSHOT_INTERVAL;
        assert_eq!(
            publisher.due(key_of(&inspector), false, after_interval),
            PublishDue::Now
        );
        let key = inspector.snapshot_key(&git);
        publisher.published(key, after_interval, publisher.next_created_at(100));
        assert_eq!(publisher.created_at, 101, "same second is stamped later");
        assert_eq!(
            publisher.due(key, false, after_interval + SNAPSHOT_INTERVAL),
            PublishDue::Idle
        );
    }

    /// Publish `inspector` at `now` if it is due, counting it in `published`.
    fn publish_if_due(
        inspector: &SessionInspector,
        publisher: &mut SnapshotPublisher,
        turn_running: bool,
        now: Instant,
        published: &mut u32,
    ) {
        let key = key_of(&inspector);
        if publisher.due(key, turn_running, now) != PublishDue::Now {
            return;
        }
        publisher.published(key, now, publisher.next_created_at(*published as u64));
        *published += 1;
    }

    /// An hour-long turn that ticks usage five times a second and moves a
    /// subagent every two seconds publishes at most once a minute, not every
    /// [`SNAPSHOT_INTERVAL`] (720 an hour). The card's target was fewer than
    /// 60 per active session-hour.
    #[test]
    fn a_streaming_turn_publishes_at_most_once_a_minute() {
        let mut inspector = busy_inspector();
        let mut publisher = SnapshotPublisher::default();
        let start = Instant::now();
        let mut published = 0;

        for tick in 0..(5 * 3600u64) {
            let now = start + Duration::from_millis(200 * tick);
            inspector.apply(&DaveApiResponse::UsageUpdate(UsageInfo {
                output_tokens: tick,
                ..Default::default()
            }));
            if tick % 10 == 0 {
                inspector.apply(&DaveApiResponse::SubagentProgress(SubagentProgress {
                    task_id: "toolu_1".into(),
                    usage: TaskUsage {
                        tool_uses: tick,
                        ..Default::default()
                    },
                    ..Default::default()
                }));
            }
            publish_if_due(&inspector, &mut publisher, true, now, &mut published);
        }

        assert_eq!(published, 60, "one snapshot a minute, the first at once");
    }

    /// Usage ticks alone never publish while the turn runs; the turn's end
    /// publishes within [`SNAPSHOT_INTERVAL`], carrying the final usage.
    #[test]
    fn a_turns_end_publishes_its_final_usage() {
        let mut inspector = busy_inspector();
        let mut publisher = SnapshotPublisher::default();
        let start = Instant::now();
        let mut published = 0;
        publish_if_due(&inspector, &mut publisher, false, start, &mut published);
        assert_eq!(published, 1);

        for tick in 1..=600u64 {
            let now = start + Duration::from_millis(200 * tick);
            inspector.apply(&DaveApiResponse::UsageUpdate(UsageInfo {
                output_tokens: tick,
                ..Default::default()
            }));
            publish_if_due(&inspector, &mut publisher, true, now, &mut published);
        }
        assert_eq!(published, 1, "two minutes of usage ticks publish nothing");

        let ended = start + Duration::from_secs(121);
        publish_if_due(&inspector, &mut publisher, false, ended, &mut published);
        assert_eq!(published, 2, "the turn's end goes out");
        assert_eq!(publisher.published, Some(key_of(&inspector)));
        assert_eq!(
            publisher.due(key_of(&inspector), false, ended + SNAPSHOT_INTERVAL),
            PublishDue::Idle
        );
    }

    /// A turn that ends right after a publish still sends its final state,
    /// once [`SNAPSHOT_INTERVAL`] has passed rather than a streaming one.
    #[test]
    fn a_turns_end_waits_only_the_settled_interval() {
        let mut inspector = busy_inspector();
        let mut publisher = SnapshotPublisher::default();
        let start = Instant::now();
        let mut published = 0;
        publish_if_due(&inspector, &mut publisher, true, start, &mut published);

        inspector.apply(&DaveApiResponse::QueryComplete(UsageInfo {
            cost_usd: Some(0.5),
            num_turns: 3,
            ..Default::default()
        }));
        let key = key_of(&inspector);
        let ended = start + Duration::from_secs(1);
        assert_eq!(
            publisher.due(key, false, ended),
            PublishDue::After(SNAPSHOT_INTERVAL - Duration::from_secs(1))
        );
        assert_eq!(
            publisher.due(key, false, start + SNAPSHOT_INTERVAL),
            PublishDue::Now
        );
    }

    /// A working-tree change republishes a session whose CLI runs, and not a
    /// dormant one, whose tree another session in its cwd may be editing.
    #[test]
    fn git_changes_count_only_while_the_cli_runs() {
        let mut inspector = SessionInspector::default();
        let mut git = GitStatusCache::new("/tmp/project".into());
        git.land_for_test(1, 0);
        assert_eq!(
            inspector.snapshot_key(&git),
            SnapshotKey::default(),
            "a dormant session's tree is no change"
        );

        inspector.cli = Some(CliState {
            connected: true,
            reap_at: None,
        });
        let running = inspector.snapshot_key(&git);
        git.land_for_test(1, 0);
        assert_eq!(inspector.snapshot_key(&git), running, "same tree, same key");
        git.land_for_test(2, 0);
        assert_ne!(inspector.snapshot_key(&git), running);
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
