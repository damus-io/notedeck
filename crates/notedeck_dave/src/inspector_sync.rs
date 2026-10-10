//! Remote parity for the session inspector, per frame: publish each local
//! session's kind-31990 snapshot when its Details moved, and fold the ones
//! that arrive for remote sessions. The snapshot itself and its debounce live
//! in [`crate::inspector`]; this is where Dave's sessions meet ndb.

use crate::backend::BackendType;
use crate::inspector::PublishDue;
use crate::publish::ingest_built_event;
use crate::stream_events::HostCtx;
use crate::{session_events, ChatSession, Dave};
use agentium_core::inspector_snapshot::{
    latest_snapshot, snapshot_session_id, InspectorSnapshot, AI_SESSION_DETAILS_KIND,
};
use nostrdb::Transaction;
use std::time::{Duration, Instant};

impl Dave {
    /// Publish a snapshot for every local session whose inspector moved since
    /// its last, at most once every
    /// [`SNAPSHOT_INTERVAL`](crate::inspector::SNAPSHOT_INTERVAL) each. One
    /// held back by the interval schedules a wake for when it is due, so the
    /// burst's last change goes out even if nothing else wakes Dave.
    pub(crate) fn publish_inspector_snapshots(&mut self, host: &HostCtx<'_>) {
        let Some(sk) = host.secret_key else {
            return;
        };
        let now = Instant::now();
        let now_secs = session_events::now_secs();
        let mut wait: Option<Duration> = None;

        for session in self.session_manager.iter_mut() {
            if session.is_remote() || session.agentic.is_none() {
                continue;
            }
            refresh_host_reads(session, now);

            let key = session.inspector.snapshot_key();
            match session.inspector.publisher.due(key, now) {
                PublishDue::Idle => continue,
                PublishDue::After(left) => {
                    wait = Some(wait.map_or(left, |w| w.min(left)));
                    continue;
                }
                PublishDue::Now => {}
            }

            let Some(agentic) = &session.agentic else {
                continue;
            };
            let inspector = &mut session.inspector;
            let created_at = inspector.publisher.next_created_at(now_secs);
            let built = inspector.snapshot(self.rate_limit).build_event(
                agentic.event_session_id(),
                created_at,
                &sk,
            );
            ingest_built_event(built, "publishing inspector snapshot", host.ndb, &sk);
            // Marked published even when the build failed, so a snapshot that
            // can't be built isn't retried every frame; the next change tries
            // again.
            inspector.publisher.published(key, now, created_at);
        }

        if let Some(wait) = wait {
            self.schedule_snapshot_wake(host.waker.clone(), now + wait);
        }
    }

    /// Wake Dave at `at`, unless a wake already scheduled will. A sleeping
    /// thread rather than a repaint request, because the headless host has no
    /// egui context to ask.
    fn schedule_snapshot_wake(&mut self, waker: notedeck::Waker, at: Instant) {
        let now = Instant::now();
        if self.snapshot_wake_at.is_some_and(|pending| pending > now) {
            return;
        }
        self.snapshot_wake_at = Some(at);
        let sleep = at.saturating_duration_since(now);
        let spawned = std::thread::Builder::new()
            .name("dave-snapshot-wake".into())
            .spawn(move || {
                std::thread::sleep(sleep);
                waker.wake();
            });
        if let Err(err) = spawned {
            tracing::warn!("failed to schedule inspector snapshot wake: {err}");
            self.snapshot_wake_at = None;
        }
    }

    /// Subscribe to `account`'s kind-31990 snapshots, which
    /// [`Self::poll_inspector_snapshots`] drains.
    pub(crate) fn subscribe_inspector_snapshots(
        &mut self,
        ndb: &nostrdb::Ndb,
        account: nostrdb_net::Pubkey,
    ) {
        let filter = nostrdb::Filter::new()
            .kinds([AI_SESSION_DETAILS_KIND as u64])
            .authors([account.bytes()])
            .build();
        match ndb.subscribe(&[filter]) {
            Ok(sub) => self.inspector_snapshot_sub = Some(sub),
            Err(e) => tracing::warn!("failed to subscribe for inspector snapshots: {e:?}"),
        }
    }

    /// Fold the snapshots that arrived for remote sessions, newest revision
    /// only, and look up the stored one for the remote session on screen the
    /// first time it is shown.
    pub(crate) fn poll_inspector_snapshots(&mut self, host: &HostCtx<'_>) {
        let Some(sub) = self.inspector_snapshot_sub else {
            return;
        };
        let account = host.author;
        let note_keys = host.ndb.poll_for_notes(sub, 32);
        let needs_lookup = self
            .session_manager
            .get_active()
            .is_some_and(|s| s.is_remote() && !s.inspector.remote.looked_up);
        if note_keys.is_empty() && !needs_lookup {
            return;
        }
        let Ok(txn) = Transaction::new(host.ndb) else {
            return;
        };

        for key in note_keys {
            let Ok(note) = host.ndb.get_note_by_key(&txn, key) else {
                continue;
            };
            if note.kind() != AI_SESSION_DETAILS_KIND || *note.pubkey() != *account.bytes() {
                continue;
            }
            let Some(session_id) = snapshot_session_id(&note) else {
                continue;
            };
            let Some(session) = self
                .session_manager
                .iter_mut()
                .find(|s| is_remote_session(s, session_id))
            else {
                continue;
            };
            let Some(snapshot) = InspectorSnapshot::from_note(&note) else {
                tracing::warn!("unreadable inspector snapshot for {session_id}");
                continue;
            };
            fold_snapshot(session, note.created_at(), snapshot);
        }

        if !needs_lookup {
            return;
        }
        let Some(session) = self.session_manager.get_active_mut() else {
            return;
        };
        session.inspector.remote.looked_up = true;
        let Some(agentic) = &session.agentic else {
            return;
        };
        let stored = latest_snapshot(host.ndb, &txn, account.bytes(), agentic.event_session_id());
        if let Some(stored) = stored {
            fold_snapshot(session, stored.created_at, stored.snapshot);
        }
    }
}

/// Whether `session` is a remote session whose kind-31988 d-tag is
/// `session_id`.
fn is_remote_session(session: &ChatSession, session_id: &str) -> bool {
    session.is_remote()
        && session
            .agentic
            .as_ref()
            .is_some_and(|a| a.event_session_id() == session_id)
}

/// Fold a snapshot revision stamped `created_at` into a remote session's
/// inspector, unless it already has a newer one.
fn fold_snapshot(session: &mut ChatSession, created_at: u64, snapshot: InspectorSnapshot) {
    if session.inspector.remote.accept(created_at) {
        session.inspector.restore(snapshot);
    }
}

/// Keep a running Claude session's temp-dir listing and commit list fresh
/// for its snapshot, as the Details tab does while it is drawn. Only while
/// the CLI runs: a dormant session writes no files and makes no commits, and
/// its last listing stays in its last snapshot.
fn refresh_host_reads(session: &mut ChatSession, now: Instant) {
    if session.backend_type != BackendType::Claude
        || !session.inspector.cli.is_some_and(|cli| cli.connected)
    {
        return;
    }
    let (Some(agentic), Some(cwd)) = (&session.agentic, session.details.cwd.as_deref()) else {
        return;
    };
    let inspector = &mut session.inspector;
    inspector.set_session_id(agentic.event_session_id());
    let cli_session_id = agentic
        .session_info
        .as_ref()
        .and_then(|info| info.claude_session_id.as_deref())
        .or(agentic.resume_session_id.as_deref());
    if let Some(cli_session_id) = cli_session_id {
        inspector.files.poll(cwd, cli_session_id, now);
    }
    if let Some(session_ref) = inspector.session_ref.as_deref() {
        inspector
            .outputs
            .commits
            .poll(cwd, session_ref, session.chat.len(), now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{
        BackgroundEntry, BackgroundTask, BackgroundWork, CliState, DaveApiResponse, UsageInfo,
    };
    use crate::session::{SessionId, SessionSource};
    use crate::test_support::TestDave;
    use nostrdb::{Filter, Ndb, SubscriptionStream};
    use std::time::Duration;

    /// The session both devices know, by its kind-31988 d-tag.
    const SESSION: &str = "inspector-snapshot-test";

    const INDEX_TIMEOUT: Duration = Duration::from_secs(10);

    /// One session on `dave` under [`SESSION`], from `source`.
    fn add_session(dave: &mut TestDave, source: SessionSource) -> SessionId {
        let [sid] = dave.add_sessions(["/tmp/inspector-snapshot"], BackendType::Claude);
        let session = dave.dave.session_manager.get_mut(sid).unwrap();
        session.agentic.as_mut().unwrap().event_id = SESSION.to_string();
        session.source = source;
        sid
    }

    /// What the host's backend streamed: init, usage over one turn, one
    /// background task.
    fn stream_into(session: &mut ChatSession) {
        let inspector = &mut session.inspector;
        inspector.apply(&DaveApiResponse::CliState(CliState {
            connected: false,
            reap_at: None,
        }));
        inspector.apply(&DaveApiResponse::UsageUpdate(UsageInfo {
            input_tokens: 12,
            cache_read_input_tokens: 40_000,
            output_tokens: 300,
            ..Default::default()
        }));
        inspector.apply(&DaveApiResponse::QueryComplete(UsageInfo {
            cost_usd: Some(1.5),
            num_turns: 3,
            ..Default::default()
        }));
        inspector.apply(&DaveApiResponse::BackgroundWork(vec![BackgroundEntry {
            id: "bash-1".into(),
            started_at: 5,
            work: BackgroundWork::Task(BackgroundTask {
                description: "cargo test".into(),
                ..Default::default()
            }),
        }]));
    }

    /// Publish the host session's snapshot and return the PNS envelope it
    /// went out in, as a relay would hand it to another device.
    async fn publish(host: &mut TestDave) -> String {
        let mut envelopes = SubscriptionStream::new(
            host.ndb.clone(),
            host.ndb
                .subscribe(&[Filter::new()
                    .kinds([nostrdb_net::pns::PNS_KIND as u64])
                    .build()])
                .unwrap(),
        );
        let parts = host.parts();
        parts.dave.publish_inspector_snapshots(&parts.host);
        let keys = envelopes
            .wait_for_notes(1, INDEX_TIMEOUT)
            .await
            .expect("the snapshot was never published");
        let txn = Transaction::new(&host.ndb).unwrap();
        let envelope = host.ndb.get_note_by_key(&txn, keys[0]).unwrap();
        format!("[\"EVENT\",\"_pns\",{}]", envelope.json().unwrap())
    }

    /// Store `envelope` on `ndb` and wait until the snapshot in it is
    /// indexed.
    async fn deliver(ndb: &Ndb, envelope: &str) {
        let mut snapshots = SubscriptionStream::new(
            ndb.clone(),
            ndb.subscribe(&[Filter::new()
                .kinds([AI_SESSION_DETAILS_KIND as u64])
                .build()])
                .unwrap(),
        );
        ndb.process_event(envelope).unwrap();
        snapshots
            .wait_for_notes(1, INDEX_TIMEOUT)
            .await
            .expect("the snapshot never unwrapped");
    }

    /// A snapshot built on the host, carried to an observer's ndb and folded
    /// there leaves the observer's inspector showing what the host's does.
    #[tokio::test]
    async fn observer_folds_the_hosts_snapshot() {
        let mut host = TestDave::new();
        let host_sid = add_session(&mut host, SessionSource::Local);
        stream_into(host.dave.session_manager.get_mut(host_sid).unwrap());
        let envelope = publish(&mut host).await;

        let mut observer = TestDave::new();
        let observer_sid = add_session(&mut observer, SessionSource::Remote);
        observer
            .dave
            .subscribe_inspector_snapshots(&observer.ndb, observer.account);
        deliver(&observer.ndb, &envelope).await;
        let parts = observer.parts();
        parts.dave.poll_inspector_snapshots(&parts.host);

        let expected = host
            .dave
            .session_manager
            .get(host_sid)
            .unwrap()
            .inspector
            .snapshot(None);
        let seen = &observer
            .dave
            .session_manager
            .get(observer_sid)
            .unwrap()
            .inspector;
        assert_eq!(seen.snapshot(None), expected);
        assert_eq!(seen.usage.total_cost(), Some(1.5));
        assert_eq!(seen.background.entries()[0].id, "bash-1");
    }

    /// A remote session opened after its snapshot arrived looks the stored
    /// one up the first time it is on screen.
    #[tokio::test]
    async fn a_remote_session_shown_later_looks_its_snapshot_up() {
        let mut host = TestDave::new();
        let host_sid = add_session(&mut host, SessionSource::Local);
        stream_into(host.dave.session_manager.get_mut(host_sid).unwrap());
        let envelope = publish(&mut host).await;

        let mut observer = TestDave::new();
        deliver(&observer.ndb, &envelope).await;
        // Subscribed only now: the poll never sees the stored note.
        observer
            .dave
            .subscribe_inspector_snapshots(&observer.ndb, observer.account);
        let observer_sid = add_session(&mut observer, SessionSource::Remote);
        let parts = observer.parts();
        parts.dave.poll_inspector_snapshots(&parts.host);

        let seen = &observer
            .dave
            .session_manager
            .get(observer_sid)
            .unwrap()
            .inspector;
        assert!(seen.remote.looked_up);
        assert_eq!(seen.usage.turns_recorded(), 1);
    }

    /// A host whose sessions nothing happened to publishes nothing, and one
    /// that just published waits out the interval.
    #[test]
    fn host_publishes_only_on_change_and_at_most_once_per_interval() {
        let mut host = TestDave::new();
        let sid = add_session(&mut host, SessionSource::Local);
        fn inspector(
            host: &mut TestDave,
            sid: SessionId,
        ) -> &mut crate::inspector::SessionInspector {
            &mut host.dave.session_manager.get_mut(sid).unwrap().inspector
        }

        let parts = host.parts();
        parts.dave.publish_inspector_snapshots(&parts.host);
        let untouched = inspector(&mut host, sid);
        let key = untouched.snapshot_key();
        assert_eq!(
            untouched.publisher.due(key, Instant::now()),
            PublishDue::Idle
        );

        stream_into(host.dave.session_manager.get_mut(sid).unwrap());
        let parts = host.parts();
        parts.dave.publish_inspector_snapshots(&parts.host);
        let published = inspector(&mut host, sid);
        let key = published.snapshot_key();
        assert_eq!(
            published.publisher.due(key, Instant::now()),
            PublishDue::Idle
        );

        published.apply(&DaveApiResponse::UsageUpdate(UsageInfo::default()));
        let key = published.snapshot_key();
        assert!(matches!(
            published.publisher.due(key, Instant::now()),
            PublishDue::After(_)
        ));
    }
}
