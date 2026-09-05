use enostr::{NormRelayUrl, NoteId, Pubkey, RelayUrlSource};
use futures_util::Stream;
use hashbrown::{HashMap, HashSet};
use nostrdb::{Error, Filter, Ndb, SubscriptionStream};
use std::{
    pin::Pin,
    task::{Context, Poll},
};

use super::{
    send_routed_relays, PlannedRoutedRelay, SendAuthorOutboxPlanConfig,
    SendAuthorOutboxPlanJobResult, SendPlanFilter, SendPlannedRoutedRelay,
};
use crate::author_outbox::{
    thread::ThreadSnapshot, RelayDirectoryRead, RelayDirectorySnapshot, RelayDirectoryState,
    RoutedRelayPriority,
};

/// Data read by one thread-planning job, used to request and watch missing ancestry.
#[derive(Debug, Default)]
pub(super) struct ThreadPlanSnapshot {
    /// Exact thread IDs encountered by this job, including missing ancestors.
    pub(super) note_ids: HashSet<NoteId>,
    /// Authors whose relay-list changes can affect these routes.
    pub(super) authors: HashSet<Pubkey>,
    /// Exact IDs that the account-read baseline still needs to fetch.
    pub(super) missing_ids: HashSet<NoteId>,
    /// Missing IDs without a claimed author, eligible for bootstrap exact-ID fetches.
    pub(super) missing_ids_without_author: HashSet<NoteId>,
}

/// Runtime-owned subscription retained across planning jobs.
/// The stream queues arrivals during a job and unsubscribes when dropped.
pub(super) struct ThreadWatch {
    note_ids: HashSet<NoteId>,
    authors: HashSet<Pubkey>,
    stream: SubscriptionStream,
}

impl ThreadWatch {
    /// Subscribe before the runtime schedules the job that catches up this coverage.
    pub(super) fn new(
        ndb: &Ndb,
        note_ids: HashSet<NoteId>,
        authors: HashSet<Pubkey>,
    ) -> Result<Self, Error> {
        if note_ids.is_empty() {
            return Err(Error::SubscriptionError);
        }
        let mut filters = vec![Filter::new()
            .ids(note_ids.iter().map(NoteId::bytes))
            .build()];
        if !authors.is_empty() {
            filters.push(
                Filter::new()
                    .authors(authors.iter().map(Pubkey::bytes))
                    .kinds([10002])
                    .build(),
            );
        }
        let stream = ndb.subscribe(&filters)?.stream(ndb).notes_per_await(64);
        Ok(Self {
            note_ids,
            authors,
            stream,
        })
    }

    /// Keep the existing subscription and its queued arrivals when coverage suffices.
    pub(super) fn covers(&self, note_ids: &HashSet<NoteId>, authors: &HashSet<Pubkey>) -> bool {
        note_ids.is_subset(&self.note_ids) && authors.is_subset(&self.authors)
    }
}

impl Stream for ThreadWatch {
    type Item = ();

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.stream)
            .poll_next(cx)
            .map(|batch| batch.map(|_| ()))
    }
}

/// Build thread routes from available ancestors, relay hints, and author lists.
///
/// Context authors select relays without narrowing the requested event authors.
/// Available routes are returned even when other authors still need discovery.
#[profiling::function]
pub(super) fn build_thread_plan(
    ndb: Ndb,
    input: SendAuthorOutboxPlanConfig,
    seeds: HashSet<NoteId>,
) -> SendAuthorOutboxPlanJobResult {
    let snapshot = match ThreadSnapshot::load(&ndb, &seeds) {
        Ok(snapshot) => snapshot,
        Err(err) => {
            return SendAuthorOutboxPlanJobResult {
                live_routed_relays: Vec::new(),
                full_history_routed_relays: Vec::new(),
                missing_authors: HashSet::new(),
                thread: Some(Err(err)),
            };
        }
    };
    let directory = RelayDirectorySnapshot::from_ndb_authors(&ndb, &snapshot.authors);
    let missing_authors = directory.missing_authors(&snapshot.authors);
    let mut relays = snapshot.relays;
    for author in &snapshot.authors {
        if let RelayDirectoryState::Known(author_relays) = directory.author_state(author) {
            relays.extend(author_relays.iter().cloned());
        }
    }
    relays.retain(|relay| {
        !input.account_read_relays.contains(relay)
            && relay.allowed_for_source(RelayUrlSource::RemoteAdvertised)
    });
    let mut relays = relays.into_iter().collect::<Vec<_>>();
    relays.sort_unstable();

    let mut missing_ids = snapshot.missing_ids.iter().copied().collect::<Vec<_>>();
    missing_ids.sort_unstable_by_key(|id| *id.bytes());
    let missing_filter = (!missing_ids.is_empty()).then(|| {
        Filter::new()
            .ids(missing_ids.iter().map(NoteId::bytes))
            .build()
    });

    SendAuthorOutboxPlanJobResult {
        live_routed_relays: thread_routes(&relays, &input.live_filters, missing_filter.as_ref()),
        full_history_routed_relays: thread_routes(&relays, &input.full_history_filters, None),
        missing_authors,
        thread: Some(Ok(ThreadPlanSnapshot {
            note_ids: snapshot.note_ids,
            authors: snapshot.authors,
            missing_ids: snapshot.missing_ids,
            missing_ids_without_author: snapshot.missing_ids_without_author,
        })),
    }
}

/// Copy unchanged thread filters to each relay, retaining filter membership.
///
/// Routed live state uses map membership to distinguish demand from a removed
/// route. Empty author sets suffice because these filters remain unmodified;
/// context authors only select relays and need not be copied to every route.
fn thread_routes(
    relays: &[NormRelayUrl],
    filters: &[SendPlanFilter],
    missing_filter: Option<&Filter>,
) -> Vec<SendPlannedRoutedRelay> {
    if filters.is_empty() && missing_filter.is_none() {
        return Vec::new();
    }

    let mut filters = filters.iter().collect::<Vec<_>>();
    filters.sort_unstable_by_key(|filter| filter.filter_index);
    let mut route_filters = filters
        .iter()
        .map(|filter| filter.filter.as_filter().clone())
        .collect::<Vec<_>>();
    let mut authors_by_filter_index = filters
        .iter()
        .map(|filter| (filter.filter_index, HashSet::new()))
        .collect::<HashMap<usize, HashSet<Pubkey>>>();
    if let Some(missing) = missing_filter {
        let filter_index = filters.last().map_or(0, |filter| filter.filter_index + 1);
        route_filters.push(missing.clone());
        authors_by_filter_index.insert(filter_index, HashSet::new());
    }

    let routes = relays
        .iter()
        .enumerate()
        .map(|(order, relay)| PlannedRoutedRelay {
            relay: relay.clone(),
            // Every route carries the same thread filters, so each contributes
            // one thread's relay coverage regardless of its context authors.
            relay_priority: RoutedRelayPriority {
                connection_weight: 1,
                order,
            },
            filters: route_filters.clone(),
            authors_by_filter_index: authors_by_filter_index.clone(),
        })
        .collect();
    let mut routes = send_routed_relays(routes);
    for route in &mut routes {
        route
            .authors_by_filter_index
            .sort_unstable_by_key(|(filter_index, _)| *filter_index);
    }
    routes
}

#[test]
fn thread_plan_retains_missing_ids_without_authors_for_bootstrap_fetches() {
    use nostrdb::{Config, NoteBuilder, Transaction};
    use std::time::{Duration, Instant};

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let ndb = Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
    let root = NoteId::new([11; 32]);
    let parent = NoteId::new([12; 32]);
    let baseline = NormRelayUrl::new("wss://baseline.example.com").expect("relay");
    let hint = NormRelayUrl::new("wss://hint.example.com/inbox").expect("relay");
    let selected = NoteBuilder::new()
        .kind(1)
        .created_at(2)
        .content("selected reply")
        .start_tag()
        .tag_str("e")
        .tag_id(root.bytes())
        .tag_str(baseline.as_str())
        .tag_str("root")
        .tag_id(&[2; 32])
        .start_tag()
        .tag_str("e")
        .tag_id(parent.bytes())
        .tag_str(hint.as_str())
        .tag_str("reply")
        .sign(&[1; 32])
        .build()
        .expect("selected");
    ndb.process_client_event(&selected.json().expect("json"))
        .expect("ingest selected");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let txn = Transaction::new(&ndb).expect("txn");
        if ndb.get_note_by_id(&txn, selected.id()).is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "note was not ingested");
        std::thread::sleep(Duration::from_millis(10));
    }
    let result = build_thread_plan(
        ndb,
        SendAuthorOutboxPlanConfig {
            account_read_relays: HashSet::from([baseline]),
            live_filters: Vec::new(),
            full_history_filters: Vec::new(),
        },
        HashSet::from([root, NoteId::new(*selected.id())]),
    );
    let snapshot = result.thread.expect("thread").expect("snapshot");
    assert_eq!(snapshot.missing_ids, HashSet::from([root, parent]));
    assert_eq!(snapshot.missing_ids_without_author, HashSet::from([parent]));
}

#[test]
fn thread_plan_preserves_reply_filters_and_fetches_missing_parents_by_exact_id() {
    use super::{SendAuthorOutboxPlanConfig, SendPlanFilter};
    use enostr::{NormRelayUrl, NoteId};
    use hashbrown::HashSet;
    use nostrdb::{Config, Filter, IngestMetadata, Ndb, NoteBuilder, SendFilter, Transaction};
    use std::time::{Duration, Instant};

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let ndb = Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
    let root = NoteId::new([11; 32]);
    let parent = NoteId::new([12; 32]);
    let baseline = NormRelayUrl::new("wss://baseline.example.com").expect("relay");
    let author_relay = NormRelayUrl::new("wss://author.example.com").expect("relay");
    let parent_hint = NormRelayUrl::new("wss://parent.example.com/inbox").expect("relay");
    let selected = NoteBuilder::new()
        .kind(1)
        .created_at(1)
        .content("reply")
        .start_tag()
        .tag_str("e")
        .tag_id(root.bytes())
        .tag_str(baseline.as_str())
        .tag_str("root")
        .start_tag()
        .tag_str("e")
        .tag_id(parent.bytes())
        .tag_str(parent_hint.as_str())
        .tag_str("reply")
        .sign(&[1; 32])
        .build()
        .expect("selected note");
    let relay_list = NoteBuilder::new()
        .kind(10002)
        .created_at(2)
        .content("")
        .start_tag()
        .tag_str("r")
        .tag_str(author_relay.as_str())
        .tag_str("write")
        .start_tag()
        .tag_str("r")
        .tag_str("wss://read-only.example.com")
        .tag_str("read")
        .sign(&[1; 32])
        .build()
        .expect("relay list");
    for note in [&selected, &relay_list] {
        ndb.process_event_with(
            &note.json().expect("json"),
            IngestMetadata::new().client(true).relay(baseline.as_str()),
        )
        .expect("ingest note");
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let txn = Transaction::new(&ndb).expect("txn");
        if [&selected, &relay_list]
            .into_iter()
            .all(|note| ndb.get_note_by_id(&txn, note.id()).is_ok())
        {
            break;
        }
        assert!(Instant::now() < deadline, "thread notes were not ingested");
        std::thread::sleep(Duration::from_millis(10));
    }

    let replies = Filter::new()
        .kinds([1])
        .event(root.bytes())
        .limit(500)
        .build();
    let root_filter = Filter::new().ids([root.bytes()]).limit(1).build();
    let history = Filter::new().kinds([1]).event(root.bytes()).build();
    let result = build_thread_plan(
        ndb,
        SendAuthorOutboxPlanConfig {
            account_read_relays: HashSet::from([baseline]),
            live_filters: [replies.clone(), root_filter.clone()]
                .into_iter()
                .enumerate()
                .map(|(filter_index, filter)| SendPlanFilter {
                    filter_index,
                    filter: SendFilter::try_from_filter(filter).expect("send filter"),
                })
                .collect(),
            full_history_filters: vec![SendPlanFilter {
                filter_index: 0,
                filter: SendFilter::try_from_filter(history.clone()).expect("send filter"),
            }],
        },
        HashSet::from([root, NoteId::new(*selected.id())]),
    );

    assert!(result.missing_authors.is_empty());
    assert_eq!(
        result
            .live_routed_relays
            .iter()
            .map(|route| &route.relay)
            .collect::<Vec<_>>(),
        vec![&author_relay, &parent_hint]
    );
    for route in &result.live_routed_relays {
        assert_eq!(route.filters.len(), 3);
        assert_eq!(
            route.filters[0].as_filter().json().expect("json"),
            replies.json().expect("json")
        );
        assert_eq!(
            route.filters[1].as_filter().json().expect("json"),
            root_filter.json().expect("json")
        );
        let missing: serde_json::Value =
            serde_json::from_str(&route.filters[2].as_filter().json().expect("json"))
                .expect("filter json");
        assert_eq!(
            missing,
            serde_json::json!({ "ids": [root.hex(), parent.hex()] })
        );
        assert_eq!(route.authors_by_filter_index.len(), 3);
    }
    assert_eq!(result.full_history_routed_relays.len(), 2);
    for route in &result.full_history_routed_relays {
        assert_eq!(route.filters.len(), 1);
        assert_eq!(
            route.filters[0].as_filter().json().expect("json"),
            history.json().expect("json")
        );
    }
    let thread = result
        .thread
        .expect("thread result")
        .expect("thread snapshot");
    assert_eq!(thread.missing_ids, HashSet::from([root, parent]));
    assert!(thread.note_ids.contains(&root));
    assert!(thread.note_ids.contains(&parent));
}

#[test]
fn thread_watch_tracks_late_notes_and_author_relay_lists_and_releases_its_subscription() {
    use futures_util::{FutureExt, StreamExt};
    use nostrdb::{Config, NoteBuilder};
    use std::time::{Duration, Instant};

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let ndb = Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
    let parent = NoteBuilder::new()
        .kind(1)
        .created_at(1)
        .content("late parent")
        .sign(&[2; 32])
        .build()
        .expect("parent note");
    let selected = NoteBuilder::new()
        .kind(1)
        .created_at(2)
        .content("late selected")
        .start_tag()
        .tag_str("e")
        .tag_id(parent.id())
        .tag_str("")
        .tag_str("reply")
        .sign(&[1; 32])
        .build()
        .expect("selected note");
    let seeds = HashSet::from([NoteId::new(*selected.id())]);
    let wait_for_change = |watch: &mut ThreadWatch| {
        let deadline = Instant::now() + Duration::from_secs(2);
        while watch.next().now_or_never().is_none() {
            assert!(Instant::now() < deadline, "thread watch missed ingestion");
            std::thread::sleep(Duration::from_millis(10));
        }
    };

    let snapshot = ThreadSnapshot::load(&ndb, &seeds).expect("missing selected snapshot");
    assert_eq!(snapshot.missing_ids, seeds);
    let mut watch = ThreadWatch::new(&ndb, snapshot.note_ids, snapshot.authors).expect("watch");
    assert_eq!(ndb.subscription_count(), 1);
    assert!(watch.next().now_or_never().is_none());
    ndb.process_client_event(&selected.json().expect("json"))
        .expect("ingest selected");
    wait_for_change(&mut watch);
    drop(watch);
    assert_eq!(ndb.subscription_count(), 0);

    let snapshot = ThreadSnapshot::load(&ndb, &seeds).expect("missing parent snapshot");
    assert_eq!(
        snapshot.missing_ids,
        HashSet::from([NoteId::new(*parent.id())])
    );
    let mut watch = ThreadWatch::new(&ndb, snapshot.note_ids, snapshot.authors).expect("watch");
    ndb.process_client_event(&parent.json().expect("json"))
        .expect("ingest parent");
    wait_for_change(&mut watch);
    drop(watch);

    let snapshot = ThreadSnapshot::load(&ndb, &seeds).expect("available parents snapshot");
    assert!(snapshot.missing_ids.is_empty());
    assert!(snapshot.authors.contains(&Pubkey::new(*parent.pubkey())));
    let mut watch = ThreadWatch::new(&ndb, snapshot.note_ids, snapshot.authors).expect("watch");
    let relay_list = NoteBuilder::new()
        .kind(10002)
        .created_at(3)
        .content("")
        .start_tag()
        .tag_str("r")
        .tag_str("wss://late-author.example.com")
        .tag_str("write")
        .sign(&[2; 32])
        .build()
        .expect("relay list");
    ndb.process_client_event(&relay_list.json().expect("json"))
        .expect("ingest relay list");
    wait_for_change(&mut watch);
    drop(watch);
    assert_eq!(ndb.subscription_count(), 0);
}

#[test]
fn thread_plans_catch_up_after_subscription_setup_and_retain_inflight_arrivals() {
    use super::{AuthorOutboxPlanAdvanceRequest, AuthorOutboxPlanRuntime};
    use crate::scoped_subs::{
        config::{ResolvedSubScope, ScopedSubKey, SubKey},
        ScopedSubEffect, SubConfig,
    };
    use enostr::OutboxIdRegistry;
    use futures_util::FutureExt;
    use nostrdb::{Config, Note, NoteBuilder, Transaction};
    use std::{
        future::Future,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        task::{Wake, Waker},
        time::{Duration, Instant},
    };

    // Register a real waker: ingestion, not an advancing timer, must wake the bridge.
    struct IngestionWake(AtomicBool);
    impl Wake for IngestionWake {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::Release);
        }
    }

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let ndb = Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
    let relay_list = |created_at, relay: &str| {
        NoteBuilder::new()
            .kind(10002)
            .created_at(created_at)
            .content("")
            .start_tag()
            .tag_str("r")
            .tag_str(relay)
            .tag_str("write")
            .sign(&[3; 32])
            .build()
            .expect("relay list")
    };
    let first_list = relay_list(1, "wss://ancestor-one.example.com");
    let second_list = relay_list(2, "wss://ancestor-two.example.com");
    let third_list = relay_list(3, "wss://ancestor-three.example.com");
    let ancestor = NoteId::new([31; 32]);
    let parent = NoteBuilder::new()
        .kind(1)
        .content("parent")
        .start_tag()
        .tag_str("e")
        .tag_id(ancestor.bytes())
        .tag_str("")
        .tag_str("root")
        .tag_id(first_list.pubkey())
        .sign(&[2; 32])
        .build()
        .expect("parent");
    let selected = NoteBuilder::new()
        .kind(1)
        .content("selected reply")
        .start_tag()
        .tag_str("e")
        .tag_id(parent.id())
        .tag_str("wss://hint.example.com/inbox")
        .tag_str("reply")
        .sign(&[1; 32])
        .build()
        .expect("selected");
    let ingest = |note: &Note<'_>| {
        ndb.process_client_event(&note.json().expect("JSON"))
            .expect("ingest");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let txn = Transaction::new(&ndb).expect("txn");
            if ndb.get_note_by_id(&txn, note.id()).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "note was not ingested");
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    let next_job = |effects: super::ScopedSubEffects| {
        let mut jobs = effects.into_effects();
        assert_eq!(jobs.len(), 1, "exactly one follow-up job");
        let ScopedSubEffect::StartAuthorOutboxPlanJob(job) = jobs.pop().expect("job");
        job
    };
    let has_relay = |runtime: &AuthorOutboxPlanRuntime, relay: &str| {
        runtime
            .slots
            .values()
            .next()
            .expect("slot")
            .ready_plan()
            .expect("ready")
            .routes
            .live_routed_relays
            .iter()
            .any(|route| route.relay.as_str() == relay)
    };
    ingest(&selected);
    let selected_id = NoteId::new(*selected.id());
    let spec = SubConfig::builder(vec![Filter::new().ids([selected.id()]).build()])
        .accounts_read_important()
        .with_author_outbox_augmentation()
        .for_thread(selected_id, [])
        .build();
    let reads = HashSet::new();
    let ids = OutboxIdRegistry::new();
    let scoped = ScopedSubKey {
        scope: ResolvedSubScope::Global,
        key: SubKey::new("thread-setup-races"),
    };
    let mut runtime = AuthorOutboxPlanRuntime::default();
    let job = next_job(
        runtime
            .advance(AuthorOutboxPlanAdvanceRequest {
                account_pubkey: Pubkey::new([9; 32]),
                scoped: scoped.clone(),
                account_read_relays: &reads,
                spec: &spec,
            })
            .effects,
    );

    // The worker's result is already complete when the parent is stored.
    let initial = job.run(ndb.clone());
    assert_eq!(ndb.subscription_count(), 0);
    assert!(initial
        .result
        .thread
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .missing_ids
        .contains(&NoteId::new(*parent.id())));
    ingest(&parent);
    let (owners, _, effects) = runtime.apply_plan_slot_ready(&ids, initial, &reads, &ndb);
    assert_eq!(
        owners,
        vec![scoped.clone()],
        "first plan is usable during catch-up"
    );
    assert!(has_relay(&runtime, "wss://hint.example.com/inbox"));
    assert_eq!(ndb.subscription_count(), 1);
    let parent_snapshot = next_job(effects).run(ndb.clone());
    assert!(parent_snapshot
        .result
        .thread
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .authors
        .contains(&Pubkey::new(*first_list.pubkey())));

    // A newly discovered author's list arrives before that author is subscribed.
    ingest(&first_list);
    let (_, _, effects) = runtime.apply_plan_slot_ready(&ids, parent_snapshot, &reads, &ndb);
    assert!(
        !has_relay(&runtime, "wss://ancestor-one.example.com/"),
        "completed result does not change"
    );
    assert_eq!(
        ndb.subscription_count(),
        1,
        "expansion releases the old subscription"
    );
    let catch_up = next_job(effects).run(ndb.clone());
    let (_, _, effects) = runtime.apply_plan_slot_ready(&ids, catch_up, &reads, &ndb);
    assert!(
        effects.into_effects().is_empty(),
        "unchanged coverage needs no catch-up"
    );
    assert!(has_relay(&runtime, "wss://ancestor-one.example.com/"));
    assert!(
        runtime.next_deadline().is_none(),
        "idle thread has no timer"
    );
    let sub_id = runtime
        .slots
        .values()
        .next()
        .unwrap()
        .thread
        .as_ref()
        .unwrap()
        .watch
        .as_ref()
        .unwrap()
        .stream
        .sub_id();

    let wake = Arc::new(IngestionWake(AtomicBool::new(false)));
    let waker = Waker::from(wake.clone());
    let mut cx = Context::from_waker(&waker);
    let changed = {
        let mut notification = std::pin::pin!(runtime.next_thread_change());
        assert!(notification.as_mut().poll(&mut cx).is_pending());
        ingest(&second_list);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !wake.0.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "NDB did not wake the waiting bridge"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let Poll::Ready(changed) = notification.as_mut().poll(&mut cx) else {
            panic!("ingestion wake must deliver the relay-list change");
        };
        changed
    };
    let job = next_job(runtime.apply_thread_change(changed));
    let second_plan = job.run(ndb.clone());

    // Another arrival after the snapshot stays queued while the job is in flight.
    ingest(&third_list);
    assert!(runtime.next_thread_change().now_or_never().is_none());
    let (_, _, effects) = runtime.apply_plan_slot_ready(&ids, second_plan, &reads, &ndb);
    assert!(effects.into_effects().is_empty());
    assert!(has_relay(&runtime, "wss://ancestor-two.example.com/"));
    assert!(!has_relay(&runtime, "wss://ancestor-three.example.com/"));
    assert_eq!(
        runtime
            .slots
            .values()
            .next()
            .unwrap()
            .thread
            .as_ref()
            .unwrap()
            .watch
            .as_ref()
            .unwrap()
            .stream
            .sub_id(),
        sub_id,
        "retain subscription and queued arrivals"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    let changed = loop {
        if let Some(changed) = runtime.next_thread_change().now_or_never() {
            break changed;
        }
        assert!(Instant::now() < deadline, "arrival during the job was lost");
        std::thread::sleep(Duration::from_millis(2));
    };
    let job = next_job(runtime.apply_thread_change(changed));
    let (_, _, effects) = runtime.apply_plan_slot_ready(&ids, job.run(ndb.clone()), &reads, &ndb);
    assert!(effects.into_effects().is_empty());
    assert!(has_relay(&runtime, "wss://ancestor-three.example.com/"));
    assert!(!has_relay(&runtime, "wss://ancestor-two.example.com/"));
    assert!(runtime.next_deadline().is_none());
    runtime.remove_scoped(&scoped);
    assert_eq!(ndb.subscription_count(), 0);
}

#[test]
fn thread_plan_failure_retains_coverage_until_a_successful_retry() {
    use super::super::config::{ResolvedSubScope, ScopedSubKey, SubConfig, SubKey};
    use super::{
        AuthorOutboxPlanAdvanceRequest, AuthorOutboxPlanJobCompletion, AuthorOutboxPlanRuntime,
        ScopedSubEffect, THREAD_PLAN_RETRY_DELAY,
    };
    use enostr::OutboxIdRegistry;
    use futures_util::FutureExt;
    use nostrdb::{Config, NoteBuilder, Transaction};
    use std::time::{Duration, Instant};

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let ndb = Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
    let parent = NoteId::new([12; 32]);
    let selected = NoteBuilder::new()
        .kind(1)
        .created_at(1)
        .content("reply")
        .start_tag()
        .tag_str("e")
        .tag_id(parent.bytes())
        .tag_str("wss://hint.example.com")
        .tag_str("reply")
        .sign(&[1; 32])
        .build()
        .expect("note");
    let wait_for_note = |id: &[u8; 32]| {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let txn = Transaction::new(&ndb).expect("txn");
            if ndb.get_note_by_id(&txn, id).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "note was not ingested");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    ndb.process_client_event(&selected.json().expect("json"))
        .expect("ingest selected");
    wait_for_note(selected.id());

    let spec = SubConfig::builder(vec![
        Filter::new().kinds([1]).event(parent.bytes()).build(),
        Filter::new().ids([parent.bytes()]).build(),
    ])
    .accounts_read_important()
    .with_author_outbox_augmentation()
    .for_thread(parent, [NoteId::new(*selected.id())])
    .build();
    let reads = HashSet::from([NormRelayUrl::new("wss://account.example.com").expect("relay")]);
    let scoped = ScopedSubKey {
        scope: ResolvedSubScope::Global,
        key: SubKey::new("thread-plan-failed-replacement"),
    };
    let account = Pubkey::new([9; 32]);
    let ids = OutboxIdRegistry::new();
    let mut runtime = AuthorOutboxPlanRuntime::default();
    let mut initial_jobs = runtime
        .advance(AuthorOutboxPlanAdvanceRequest {
            account_pubkey: account,
            scoped: scoped.clone(),
            account_read_relays: &reads,
            spec: &spec,
        })
        .effects
        .into_effects();
    let ScopedSubEffect::StartAuthorOutboxPlanJob(job) = initial_jobs.pop().expect("initial job");
    let (_, _, effects) = runtime.apply_plan_slot_ready(&ids, job.run(ndb.clone()), &reads, &ndb);
    let mut jobs = effects.into_effects();
    assert_eq!(jobs.len(), 1, "first subscription schedules catch-up");
    let ScopedSubEffect::StartAuthorOutboxPlanJob(job) = jobs.pop().expect("catch-up job");
    let (_, _, effects) = runtime.apply_plan_slot_ready(&ids, job.run(ndb.clone()), &reads, &ndb);
    assert!(effects.into_effects().is_empty());
    let slot = runtime.slots.values().next().expect("slot");
    let initial_generation = slot.ready_plan().expect("ready").generation;
    let baseline_fetch = slot
        .thread
        .as_ref()
        .expect("thread")
        .baseline_fetch
        .id
        .expect("baseline fetch");
    assert_eq!(
        slot.ready_plan()
            .expect("ready")
            .routes
            .live_routed_relays
            .len(),
        1
    );

    let list = NoteBuilder::new()
        .kind(10002)
        .created_at(2)
        .content("")
        .start_tag()
        .tag_str("r")
        .tag_str("wss://author.example.com")
        .tag_str("write")
        .sign(&[1; 32])
        .build()
        .expect("relay list");
    ndb.process_client_event(&list.json().expect("json"))
        .expect("ingest relay list");
    wait_for_note(list.id());
    let changed = runtime
        .next_thread_change()
        .now_or_never()
        .expect("relay-list notification");
    let mut jobs = runtime.apply_thread_change(changed).into_effects();
    assert_eq!(jobs.len(), 1, "relay-list ingestion starts a replacement");
    let ScopedSubEffect::StartAuthorOutboxPlanJob(job) = jobs.pop().expect("replacement job");
    let failed = AuthorOutboxPlanJobCompletion {
        slot_id: job.slot_id,
        build_stage: job.build_stage,
        result: SendAuthorOutboxPlanJobResult {
            live_routed_relays: Vec::new(),
            full_history_routed_relays: Vec::new(),
            missing_authors: HashSet::new(),
            thread: Some(Err(nostrdb::Error::NotFound)),
        },
    };
    let (_, ops, effects) = runtime.apply_plan_slot_ready(&ids, failed, &reads, &ndb);
    assert!(
        effects.into_effects().is_empty(),
        "failure retries after its deadline"
    );
    assert!(
        ops.is_empty(),
        "a failed snapshot must not clear working fetches"
    );
    let slot = runtime.slots.values().next().expect("slot");
    assert_eq!(
        slot.ready_plan().expect("retained plan").generation,
        initial_generation
    );
    assert_eq!(
        slot.ready_plan()
            .expect("retained plan")
            .routes
            .live_routed_relays
            .len(),
        1
    );
    let thread = slot.thread.as_ref().expect("thread");
    assert_eq!(thread.baseline_fetch.id, Some(baseline_fetch));
    assert_eq!(thread.baseline_fetch.missing_ids, HashSet::from([parent]));

    let mut retry_jobs = runtime
        .apply_relay_list_discovery_retry_due(Instant::now() + THREAD_PLAN_RETRY_DELAY)
        .1
        .into_effects();
    assert_eq!(retry_jobs.len(), 1, "failed snapshot schedules another job");
    let ScopedSubEffect::StartAuthorOutboxPlanJob(retry) = retry_jobs.pop().expect("retry job");
    let (_, ops, effects) =
        runtime.apply_plan_slot_ready(&ids, retry.run(ndb.clone()), &reads, &ndb);
    assert!(
        effects.into_effects().is_empty(),
        "recovery retains the existing subscription"
    );
    assert!(
        ops.is_empty(),
        "unchanged missing IDs retain their baseline fetch"
    );
    let slot = runtime.slots.values().next().expect("slot");
    assert!(slot.ready_plan().expect("recovered plan").generation > initial_generation);
    assert_eq!(
        slot.ready_plan()
            .expect("recovered plan")
            .routes
            .live_routed_relays
            .len(),
        2
    );
    let thread = slot.thread.as_ref().expect("thread");
    assert_eq!(thread.baseline_fetch.id, Some(baseline_fetch));
    assert!(thread.watch.is_some());
    assert_eq!(ndb.subscription_count(), 1);
}

#[test]
fn thread_plan_failures_back_off_until_reads_and_subscription_recover() {
    use super::super::config::{ResolvedSubScope, ScopedSubKey, SubConfig, SubKey};
    use super::{
        AuthorOutboxPlanAdvanceRequest, AuthorOutboxPlanJobRequest, AuthorOutboxPlanRuntime,
        ScopedSubEffect, ScopedSubEffects,
    };
    use enostr::OutboxIdRegistry;
    use std::time::{Duration, Instant};

    fn next_job(effects: ScopedSubEffects) -> AuthorOutboxPlanJobRequest {
        let mut effects = effects.into_effects();
        assert_eq!(effects.len(), 1, "exactly one planning job");
        let ScopedSubEffect::StartAuthorOutboxPlanJob(job) = effects.pop().unwrap();
        job
    }

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let ndb = Ndb::new(tmp.path().to_str().expect("path"), &nostrdb::Config::new()).expect("ndb");
    let root = NoteId::new([42; 32]);
    let filters = [Filter::new().ids([root.bytes()]).build()];
    let mut blockers = Vec::new();
    while let Ok(subscription) = ndb.subscribe(&filters) {
        blockers.push(subscription.stream(&ndb));
        assert!(
            blockers.len() < 1024,
            "subscription capacity must be finite"
        );
    }
    assert!(!blockers.is_empty(), "fill NDB's subscription capacity");

    let spec = SubConfig::builder(filters.into())
        .accounts_read_important()
        .with_author_outbox_augmentation()
        .for_thread(root, [])
        .build();
    let scoped = ScopedSubKey {
        scope: ResolvedSubScope::Global,
        key: SubKey::new("thread-plan-exponential-backoff"),
    };
    let reads = HashSet::new();
    let ids = OutboxIdRegistry::new();
    let mut runtime = AuthorOutboxPlanRuntime::default();
    let mut job = next_job(
        runtime
            .advance(AuthorOutboxPlanAdvanceRequest {
                account_pubkey: Pubkey::new([1; 32]),
                scoped: scoped.clone(),
                account_read_relays: &reads,
                spec: &spec,
            })
            .effects,
    );
    let slot_id = job.slot_id;

    for attempt in 0..14 {
        let mut completion = job.run(ndb.clone());
        if attempt % 2 == 0 {
            // Exercise the same completion boundary as an NDB read error.
            completion.result.thread = Some(Err(nostrdb::Error::TransactionFailed));
        }
        // Odd attempts read successfully, but real subscription setup fails.
        let expected = Duration::from_millis(100u64 << attempt).min(Duration::from_secs(60));
        let before = Instant::now();
        let (_, _, effects) = runtime.apply_plan_slot_ready(&ids, completion, &reads, &ndb);
        let after = Instant::now();
        assert!(effects.into_effects().is_empty());
        let deadline = runtime.next_deadline().expect("retry deadline");
        assert!(
            (before + expected..=after + expected).contains(&deadline),
            "attempt {attempt} should retry after {expected:?}, got {:?}",
            deadline.saturating_duration_since(before),
        );
        assert!(
            runtime
                .apply_relay_list_discovery_retry_due(deadline - Duration::from_nanos(1))
                .1
                .into_effects()
                .is_empty(),
            "the timer cannot start a job before its deadline"
        );
        job = next_job(runtime.apply_relay_list_discovery_retry_due(deadline).1);
        assert!(runtime.next_deadline().is_none(), "no retry during a job");
    }

    drop(blockers);
    let (_, _, effects) = runtime.apply_plan_slot_ready(&ids, job.run(ndb.clone()), &reads, &ndb);
    let catch_up = next_job(effects);
    assert!(runtime.next_deadline().is_none());
    assert_eq!(
        runtime.slots[&slot_id].thread.as_ref().unwrap().retry_delay,
        Duration::from_millis(100),
        "successful subscription setup resets backoff before catch-up"
    );
    let (_, _, effects) =
        runtime.apply_plan_slot_ready(&ids, catch_up.run(ndb.clone()), &reads, &ndb);
    assert!(effects.into_effects().is_empty());
    assert_eq!(ndb.subscription_count(), 1);

    // Recovery resets backoff both after setup and with an already-covering watch.
    for _ in 0..2 {
        let mut completion = next_job(runtime.apply_thread_change(slot_id)).run(ndb.clone());
        completion.result.thread = Some(Err(nostrdb::Error::TransactionFailed));
        let before = Instant::now();
        let (_, _, effects) = runtime.apply_plan_slot_ready(&ids, completion, &reads, &ndb);
        let after = Instant::now();
        assert!(effects.into_effects().is_empty());
        let deadline = runtime.next_deadline().expect("retry after new failure");
        let base = Duration::from_millis(100);
        assert!((before + base..=after + base).contains(&deadline));
        let retry = next_job(runtime.apply_relay_list_discovery_retry_due(deadline).1);
        let (_, _, effects) =
            runtime.apply_plan_slot_ready(&ids, retry.run(ndb.clone()), &reads, &ndb);
        assert!(effects.into_effects().is_empty());
        assert!(runtime.next_deadline().is_none());
        assert_eq!(ndb.subscription_count(), 1, "retain existing coverage");
    }

    runtime.remove_scoped(&scoped);
    assert!(runtime.next_deadline().is_none());
    assert_eq!(ndb.subscription_count(), 0);
}

#[test]
fn thread_routes_retain_filter_membership_without_copying_context_authors() {
    use nostrdb::{Config, NoteBuilder, SendFilter, Transaction};
    use std::time::{Duration, Instant};

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let ndb = Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
    let parent = NoteId::new([12; 32]);
    let selected = NoteBuilder::new()
        .kind(1)
        .created_at(1)
        .content("reply")
        .start_tag()
        .tag_str("e")
        .tag_id(parent.bytes())
        .tag_str("wss://hint.example.com")
        .tag_str("reply")
        .sign(&[1; 32])
        .build()
        .expect("note");
    ndb.process_client_event(&selected.json().expect("json"))
        .expect("ingest note");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let txn = Transaction::new(&ndb).expect("txn");
        if ndb.get_note_by_id(&txn, selected.id()).is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "note was not ingested");
        std::thread::sleep(Duration::from_millis(10));
    }
    let result = build_thread_plan(
        ndb,
        SendAuthorOutboxPlanConfig {
            account_read_relays: HashSet::new(),
            live_filters: vec![SendPlanFilter {
                filter_index: 0,
                filter: SendFilter::try_from_filter(
                    Filter::new().kinds([1]).event(parent.bytes()).build(),
                )
                .expect("send filter"),
            }],
            full_history_filters: Vec::new(),
        },
        HashSet::from([NoteId::new(*selected.id())]),
    );
    assert_eq!(result.live_routed_relays.len(), 1);
    let route = &result.live_routed_relays[0];
    assert_eq!(
        route.filters.len(),
        2,
        "reply filter plus missing parent ID"
    );
    assert_eq!(
        route.authors_by_filter_index.len(),
        2,
        "each filter retains demand"
    );
    assert!(
        route
            .authors_by_filter_index
            .iter()
            .all(|(_, authors)| authors.is_empty()),
        "routing authors do not need to be copied into unmodified thread filters"
    );
}

/// A parent arriving after discovery EOSE must trigger relay-list discovery for
/// a newly revealed ancestor author, even when a snapshot was already queued.
#[test]
fn thread_plan_discovers_new_ancestor_author_after_discovery_eose() {
    use super::{AuthorOutboxPlanAdvanceRequest, AuthorOutboxPlanRuntime};
    use crate::scoped_subs::{
        config::{ResolvedSubScope, ScopedSubKey, SubKey},
        ScopedSubEffect, ScopedSubOutboxOp, SubConfig,
    };
    use enostr::{FullKeypair, OutboxIdRegistry, RelayReqStatus};
    use futures_util::FutureExt;
    use nostrdb::{Config, Note, NoteBuilder, Transaction};
    use std::time::{Duration, Instant};

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let ndb = Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
    let author = FullKeypair::generate();
    let ancestor_author = FullKeypair::generate();
    let ancestor = NoteId::new([31; 32]);
    let parent = NoteBuilder::new()
        .kind(1)
        .content("parent revealing a new ancestor author")
        .start_tag()
        .tag_str("e")
        .tag_id(ancestor.bytes())
        .tag_str("")
        .tag_str("root")
        .tag_id(ancestor_author.pubkey.bytes())
        .sign(&author.secret_key.secret_bytes())
        .build()
        .expect("parent note");
    let child = NoteBuilder::new()
        .kind(1)
        .content("selected child")
        .start_tag()
        .tag_str("e")
        .tag_id(parent.id())
        .tag_str("wss://hint.example.com/inbox")
        .tag_str("root")
        .tag_id(author.pubkey.bytes())
        .sign(&author.secret_key.secret_bytes())
        .build()
        .expect("selected note");
    let ingest = |note: &Note<'_>| {
        ndb.process_client_event(&note.json().expect("note JSON"))
            .expect("ingest note");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let txn = Transaction::new(&ndb).expect("txn");
            if ndb.get_note_by_id(&txn, note.id()).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "note was not ingested");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let discovers_author = |filter: &Filter, author: &Pubkey| {
        let value: serde_json::Value =
            serde_json::from_str(&filter.json().expect("filter JSON")).expect("parsed filter");
        value["kinds"]
            .as_array()
            .is_some_and(|kinds| kinds.contains(&serde_json::json!(10002)))
            && value["authors"]
                .as_array()
                .is_some_and(|authors| authors.contains(&serde_json::json!(author.hex())))
    };
    ingest(&child);
    let root = NoteId::new(*parent.id());
    let spec = SubConfig::builder(vec![Filter::new().ids([root.bytes()]).build()])
        .accounts_read_important()
        .with_author_outbox_augmentation()
        .for_thread(root, [NoteId::new(*child.id())])
        .build();
    let account_relay = NormRelayUrl::new("wss://account.example.com").expect("account relay");
    let reads = HashSet::from([account_relay.clone()]);
    let ids = OutboxIdRegistry::new();
    let mut runtime = AuthorOutboxPlanRuntime::default();
    let mut jobs = runtime
        .advance(AuthorOutboxPlanAdvanceRequest {
            account_pubkey: author.pubkey,
            scoped: ScopedSubKey {
                scope: ResolvedSubScope::Global,
                key: SubKey::new("thread-author-discovered-after-eose"),
            },
            account_read_relays: &reads,
            spec: &spec,
        })
        .effects
        .into_effects();
    assert_eq!(jobs.len(), 1, "initial thread snapshot");
    let ScopedSubEffect::StartAuthorOutboxPlanJob(job) = jobs.pop().expect("initial job");
    let (_, ops, effects) = runtime.apply_plan_slot_ready(&ids, job.run(ndb.clone()), &reads, &ndb);
    jobs.extend(effects.into_effects());
    assert_eq!(jobs.len(), 1, "first subscription schedules catch-up");
    let ScopedSubEffect::StartAuthorOutboxPlanJob(job) = jobs.pop().expect("catch-up job");
    let (_, catch_up_ops, effects) =
        runtime.apply_plan_slot_ready(&ids, job.run(ndb.clone()), &reads, &ndb);
    assert!(catch_up_ops.is_empty());
    assert!(effects.into_effects().is_empty());
    let discovery_id = ops
        .into_ops()
        .into_iter()
        .find_map(|op| match op {
            ScopedSubOutboxOp::StartFetch { id, filters, .. }
                if filters
                    .iter()
                    .any(|filter| discovers_author(filter, &author.pubkey)) =>
            {
                Some(id)
            }
            _ => None,
        })
        .expect("initial missing-author relay-list discovery");
    let (_, effects) =
        runtime.apply_relay_req_status(discovery_id, &account_relay, Some(RelayReqStatus::Eose));
    jobs.extend(effects.into_effects());

    assert!(jobs.is_empty(), "EOSE does not rebuild a thread plan");
    assert!(
        runtime.next_deadline().is_none(),
        "a healthy thread needs no timer"
    );
    ingest(&parent);
    let changed = runtime
        .next_thread_change()
        .now_or_never()
        .expect("parent notification");
    jobs.extend(runtime.apply_thread_change(changed).into_effects());
    assert_eq!(jobs.len(), 1, "snapshot observes the newly imported parent");
    let ScopedSubEffect::StartAuthorOutboxPlanJob(job) = jobs.pop().expect("parent snapshot");
    let (_, ops, effects) = runtime.apply_plan_slot_ready(&ids, job.run(ndb.clone()), &reads, &ndb);
    assert_eq!(
        effects.into_effects().len(),
        1,
        "expanded author coverage schedules catch-up"
    );
    assert!(
        ops.into_ops().into_iter().any(|op| match op {
            ScopedSubOutboxOp::StartFetch { filters, .. } => filters
                .iter()
                .any(|filter| discovers_author(filter, &ancestor_author.pubkey)),
            _ => false,
        }),
        "new ancestor author must receive a kind-10002 discovery request after earlier EOSE"
    );
}
