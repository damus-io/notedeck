use crate::{
    actionbar::TimelineOpenResult,
    error::Error,
    timeline::{
        drop_timeline_remote_owner, ensure_remote_timeline_subscription, InitialLoadState,
        InsertNewResult, RemoteSubscriptionPolicy, Timeline, TimelineKind,
    },
};

use notedeck::ScopedSubApi;
use notedeck::{filter, FilterState, NoteCache, NoteRef};

use enostr::Pubkey;
use nostrdb::{Filter, Ndb, Transaction};
use std::collections::HashMap;
use tracing::{debug, error, info, warn};

#[derive(Default)]
pub struct TimelineCache {
    timelines: HashMap<TimelineKind, Timeline>,
}

pub enum Vitality<'a, M> {
    Fresh(&'a mut M),
    Stale(&'a mut M),
}

impl<'a, M> Vitality<'a, M> {
    pub fn get_ptr(self) -> &'a mut M {
        match self {
            Self::Fresh(ptr) => ptr,
            Self::Stale(ptr) => ptr,
        }
    }

    pub fn is_stale(&self) -> bool {
        match self {
            Self::Fresh(_ptr) => false,
            Self::Stale(_ptr) => true,
        }
    }
}

impl<'a> IntoIterator for &'a mut TimelineCache {
    type Item = (&'a TimelineKind, &'a mut Timeline);
    type IntoIter = std::collections::hash_map::IterMut<'a, TimelineKind, Timeline>;

    fn into_iter(self) -> Self::IntoIter {
        self.timelines.iter_mut()
    }
}

impl TimelineCache {
    /// Pop a timeline from the timeline cache. This only removes the timeline
    /// if it has reached 0 subscribers, meaning it was the last one to be
    /// removed
    pub fn pop(
        &mut self,
        id: &TimelineKind,
        ndb: &mut Ndb,
        scoped_subs: &mut ScopedSubApi<'_>,
    ) -> Result<(), Error> {
        let account_pk = scoped_subs.selected_account_pubkey();
        self.pop_for_account(id, account_pk, ndb, scoped_subs)
    }

    pub(crate) fn pop_for_account(
        &mut self,
        id: &TimelineKind,
        account_pk: Pubkey,
        ndb: &mut Ndb,
        scoped_subs: &mut ScopedSubApi<'_>,
    ) -> Result<(), Error> {
        let timeline = if let Some(timeline) = self.timelines.get_mut(id) {
            timeline
        } else {
            return Err(Error::TimelineNotFound);
        };

        timeline
            .subscription
            .unsubscribe_or_decrement(account_pk, ndb);

        if timeline.subscription.no_sub(&account_pk) {
            timeline.subscription.mark_remote_pending(account_pk);
            drop_timeline_remote_owner(timeline, account_pk, scoped_subs);
            // Reset so a later reopen re-runs the initial load and picks
            // up notes posted while the timeline was closed.
            timeline.initial_load = InitialLoadState::Pending;
        }

        if !timeline.subscription.has_any_subs() {
            debug!(
                "popped last timeline {:?}, removing from timeline cache",
                id
            );
            self.timelines.remove(id);
        }

        Ok(())
    }

    fn get_expected_mut(&mut self, key: &TimelineKind) -> &mut Timeline {
        self.timelines
            .get_mut(key)
            .expect("expected notes in timline cache")
    }

    /// Insert a new timeline into the cache, based on the TimelineKind
    #[allow(clippy::too_many_arguments)]
    fn insert_new(
        &mut self,
        id: TimelineKind,
        txn: &Transaction,
        ndb: &Ndb,
        notes: &[NoteRef],
        note_cache: &mut NoteCache,
    ) -> Option<InsertNewResult> {
        let mut timeline = if let Some(timeline) = id.clone().into_timeline(txn, ndb) {
            timeline
        } else {
            error!("Error creating timeline from {:?}", &id);
            return None;
        };

        // insert initial notes into timeline
        let res = timeline.insert_new(txn, ndb, note_cache, notes);
        self.timelines.insert(id, timeline);

        Some(res)
    }

    pub fn insert(&mut self, id: TimelineKind, account_pk: Pubkey, mut timeline: Timeline) {
        if let Some(cur_timeline) = self.timelines.get_mut(&id) {
            cur_timeline.subscription.increment(account_pk);
            return;
        };

        timeline.subscription.increment(account_pk);
        self.timelines.insert(id, timeline);
    }

    /// Get and/or update the notes associated with this timeline
    #[profiling::function]
    fn notes<'a>(
        &'a mut self,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        id: &TimelineKind,
    ) -> GetNotesResponse<'a> {
        // we can't use the naive hashmap entry API here because lookups
        // require a copy, wait until we have a raw entry api. We could
        // also use hashbrown?

        if self.timelines.contains_key(id) {
            return GetNotesResponse {
                vitality: Vitality::Stale(self.get_expected_mut(id)),
                insert_result: None,
            };
        }

        let notes = if let FilterState::Ready(filters) = id.filters(txn, ndb) {
            let mut notes = Vec::new();

            for package in filters.local().packages {
                profiling::scope!("ndb query");
                if let Ok(results) = ndb.query(txn, package.filters, 1000) {
                    let cur_notes: Vec<NoteRef> = results
                        .into_iter()
                        .map(NoteRef::from_query_result)
                        .collect();

                    notes.extend(cur_notes);
                } else {
                    debug!("got no results from TimelineCache lookup for {:?}", id);
                }
            }

            notes
        } else {
            // filter is not ready yet
            vec![]
        };

        if notes.is_empty() {
            warn!("NotesHolder query returned 0 notes? ")
        } else {
            info!("found NotesHolder with {} notes", notes.len());
        }

        let insert_result = self.insert_new(id.to_owned(), txn, ndb, &notes, note_cache);

        GetNotesResponse {
            vitality: Vitality::Fresh(self.get_expected_mut(id)),
            insert_result,
        }
    }

    /// Open a timeline with a Columns-internal remote subscription policy.
    ///
    /// When `load_local` is false, the timeline is created and subscribed
    /// without running a blocking local query. Use this for startup paths
    /// where initial notes are loaded asynchronously.
    #[profiling::function]
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &mut self,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        scoped_subs: &mut ScopedSubApi<'_>,
        id: &TimelineKind,
        account_pk: Pubkey,
        load_local: bool,
        remote_policy: RemoteSubscriptionPolicy,
    ) -> Option<TimelineOpenResult> {
        if !load_local {
            let timeline = if let Some(timeline) = self.timelines.get_mut(id) {
                timeline
            } else {
                let Some(timeline) = id.clone().into_timeline(txn, ndb) else {
                    error!("Error creating timeline from {:?}", id);
                    return None;
                };
                self.timelines.insert(id.clone(), timeline);
                self.timelines.get_mut(id).expect("timeline inserted")
            };

            if let FilterState::Ready(filter) = &timeline.filter {
                debug!("got open with subscription for {:?}", &timeline.kind);
                timeline.subscription.try_add_local(account_pk, ndb, filter);
                ensure_remote_timeline_subscription(
                    timeline,
                    account_pk,
                    filter.remote().to_vec(),
                    scoped_subs,
                    remote_policy,
                );
            } else {
                debug!(
                    "open skipped subscription; filter not ready for {:?}",
                    &timeline.kind
                );
            }

            timeline.subscription.increment(account_pk);
            return None;
        }

        let account_pk = scoped_subs.selected_account_pubkey();
        let notes_resp = self.notes(ndb, note_cache, txn, id);
        let (mut open_result, timeline) = match notes_resp.vitality {
            Vitality::Stale(timeline) => {
                // The timeline cache is stale, let's update it
                let notes = collect_stale_notes(timeline, txn, ndb);

                let open_result = if notes.is_empty() {
                    None
                } else {
                    let new_notes = notes.iter().map(|n| n.key).collect();
                    Some(TimelineOpenResult::new_notes(new_notes, id.clone()))
                };

                // we can't insert and update the VirtualList now, because we
                // are already borrowing it mutably. Let's pass it as a
                // result instead
                //
                // holder.get_view().insert(&notes); <-- no
                (open_result, timeline)
            }

            Vitality::Fresh(timeline) => (None, timeline),
        };

        if let FilterState::Ready(filter) = &timeline.filter {
            debug!("got open with *new* subscription for {:?}", &timeline.kind);
            timeline.subscription.try_add_local(account_pk, ndb, filter);
            ensure_remote_timeline_subscription(
                timeline,
                account_pk,
                filter.remote().to_vec(),
                scoped_subs,
                remote_policy,
            );
        } else {
            // This should never happen reasoning, self.notes would have
            // failed above if the filter wasn't ready
            error!(
                "open: filter not ready, so could not setup subscription. this should never happen"
            );
        };

        timeline.subscription.increment(account_pk);

        if let Some(insert_result) = notes_resp.insert_result {
            match &mut open_result {
                Some(o) => o.insert_insert_result(insert_result),
                None => open_result = Some(TimelineOpenResult::new_insert_result(insert_result)),
            }
        }

        open_result
    }

    pub fn get(&self, id: &TimelineKind) -> Option<&Timeline> {
        self.timelines.get(id)
    }

    pub fn get_mut(&mut self, id: &TimelineKind) -> Option<&mut Timeline> {
        self.timelines.get_mut(id)
    }

    pub(crate) fn refresh_remote_subscriptions(
        &mut self,
        scoped_subs: &mut ScopedSubApi<'_>,
        remote_policy: RemoteSubscriptionPolicy,
    ) {
        for timeline in self.timelines.values_mut() {
            let accounts = timeline.subscription.accounts_with_dependers();
            if accounts.is_empty() {
                continue;
            }

            let Some(remote_filters) = timeline.remote_filters_for_subscription_refresh() else {
                continue;
            };

            for account_pk in accounts {
                super::update_remote_timeline_subscription_for_account(
                    timeline,
                    account_pk,
                    remote_filters.clone(),
                    scoped_subs,
                    remote_policy,
                );
            }
        }
    }

    pub fn num_timelines(&self) -> usize {
        self.timelines.len()
    }

    pub fn set_fresh(&mut self, kind: &TimelineKind) {
        let Some(tl) = self.get_mut(kind) else {
            return;
        };

        tl.seen_latest_notes = true;
    }
}

fn collect_stale_notes(timeline: &Timeline, txn: &Transaction, ndb: &Ndb) -> Vec<NoteRef> {
    let FilterState::Ready(filter) = &timeline.filter else {
        return Vec::new();
    };

    let mut notes = Vec::new();
    for package in filter.local().packages {
        let cur_notes = find_new_notes(
            timeline.all_or_any_entries().latest(),
            package.filters,
            txn,
            ndb,
        );
        notes.extend(cur_notes);
    }
    notes
}

pub struct GetNotesResponse<'a> {
    vitality: Vitality<'a, Timeline>,
    insert_result: Option<InsertNewResult>,
}

/// Look for new thread notes since our last fetch
fn find_new_notes(
    latest: Option<&NoteRef>,
    filters: &[Filter],
    txn: &Transaction,
    ndb: &Ndb,
) -> Vec<NoteRef> {
    let Some(last_note) = latest else {
        return vec![];
    };

    let filters = filter::make_filters_since(filters, last_note.created_at + 1);

    if let Ok(results) = ndb.query(txn, &filters, 1000) {
        debug!("got {} results from NotesHolder update", results.len());
        results
            .into_iter()
            .map(NoteRef::from_query_result)
            .collect()
    } else {
        debug!("got no results from NotesHolder update",);
        vec![]
    }
}

#[cfg(test)]
mod tests {
    //! Regression tests for [#1449]: posts made while a profile timeline
    //! was closed must still show up when the timeline is reopened.
    //!
    //! This simulates the exact repro:
    //!   1. post A → open profile → see A
    //!   2. back → post B
    //!   3. open profile → must see B
    //!
    //! The bug (introduced in 019f93e7018e) was that `TimelineCache::pop`
    //! left a phantom subscription entry behind, keeping the Timeline
    //! alive in the cache with `initial_load = Complete`. On reopen, the
    //! app's `schedule_timeline_load` gate would short-circuit and never
    //! query ndb for B.
    //!
    //! [#1449]: https://github.com/damus-io/notedeck/issues/1449

    use super::*;
    use crate::timeline::InitialLoadState;
    use enostr::FullKeypair;
    use nostrdb::NoteBuilder;
    use notedeck::{NoteCache, Notedeck, UnknownIds};
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    struct Harness {
        _tmp: TempDir,
        notedeck: Notedeck,
        kp: FullKeypair,
    }

    fn make_harness() -> Harness {
        let tmp = TempDir::new().expect("tmp");
        let ui_ctx = egui::Context::default();
        let kp = FullKeypair::generate();
        let notedeck = Notedeck::init(
            &ui_ctx,
            tmp.path(),
            &[
                "notedeck".to_owned(),
                "--testrunner".to_owned(),
                "--sec".to_owned(),
                hex::encode(kp.secret_key.to_secret_bytes()),
            ],
        );

        Harness {
            _tmp: tmp,
            notedeck,
            kp,
        }
    }

    /// Publish a kind-1 note authored by the harness keypair.
    fn publish_note(h: &mut Harness, content: &str, created_at: u64) {
        let note = NoteBuilder::new()
            .kind(1)
            .content(content)
            .created_at(created_at)
            .sign(&h.kp.secret_key.secret_bytes())
            .build()
            .expect("note build");
        let json = note.json().expect("note json");
        h.notedeck
            .app_context()
            .ndb
            .process_client_event(&json)
            .expect("ingest");
    }

    /// Wait until ndb has at least `n` matching notes. Ingestion is async.
    fn wait_for_count(ndb: &Ndb, filter: &[Filter], n: usize) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let txn = Transaction::new(ndb).expect("txn");
            let hit = ndb.query(&txn, filter, 100).map(|r| r.len()).unwrap_or(0);
            if hit >= n {
                return;
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting for {n} notes, have {hit}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Mirrors the subset of `app::schedule_timeline_load` + the timeline
    /// loader's ndb work that's relevant here: if the timeline's
    /// `initial_load` is `Pending`, query ndb and insert the results,
    /// then mark it `Complete`. Otherwise do nothing (that's the bug
    /// path).
    fn run_scheduled_initial_load(
        cache: &mut TimelineCache,
        kind: &TimelineKind,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        unknown_ids: &mut UnknownIds,
    ) {
        let timeline = cache.get_mut(kind).expect("timeline exists");
        if timeline.initial_load != InitialLoadState::Pending {
            return;
        }
        let FilterState::Ready(filter) = timeline.filter.clone() else {
            panic!("filter should be ready for profile timeline");
        };
        let txn = Transaction::new(ndb).expect("txn");
        let mut notes: Vec<NoteRef> = Vec::new();
        for pkg in filter.local().packages {
            let results = ndb.query(&txn, pkg.filters, 1000).expect("query ok");
            notes.extend(results.into_iter().map(NoteRef::from_query_result));
        }
        let insert_result = timeline.insert_new(&txn, ndb, note_cache, &notes);
        insert_result.process(ndb, &txn, unknown_ids, note_cache);
        timeline.initial_load = InitialLoadState::Complete;
    }

    fn remote_policy() -> RemoteSubscriptionPolicy {
        RemoteSubscriptionPolicy::from_outbox_relays(true)
    }

    #[tokio::test]
    async fn reopened_profile_shows_posts_made_while_closed() {
        let mut h = make_harness();
        let pk = h.kp.pubkey;
        let kind = TimelineKind::profile(pk);
        let author_filter = vec![Filter::new().authors([pk.bytes()]).kinds([1]).build()];

        let mut cache = TimelineCache::default();

        // --- Step 1: post A, then open profile and run the initial load ---
        publish_note(&mut h, "post A", 1_700_000_100);
        {
            let app_ctx = h.notedeck.app_context();
            wait_for_count(app_ctx.ndb, &author_filter, 1);
        }

        {
            let mut app_ctx = h.notedeck.app_context();
            let ndb = &mut *app_ctx.ndb;
            let txn = Transaction::new(ndb).expect("txn");
            let mut scoped_subs = app_ctx.remote.scoped_subs(app_ctx.accounts);
            cache.open(
                ndb,
                app_ctx.note_cache,
                &txn,
                &mut scoped_subs,
                &kind,
                pk,
                false,
                remote_policy(),
            );
        }
        {
            let app_ctx = h.notedeck.app_context();
            run_scheduled_initial_load(
                &mut cache,
                &kind,
                app_ctx.ndb,
                app_ctx.note_cache,
                app_ctx.unknown_ids,
            );
        }

        assert_eq!(
            cache
                .get(&kind)
                .expect("timeline")
                .current_view()
                .units
                .len(),
            1,
            "post A should be visible after the first open"
        );

        // --- Step 2: go back (pop the route) ---
        {
            let mut app_ctx = h.notedeck.app_context();
            let mut scoped_subs = app_ctx.remote.scoped_subs(app_ctx.accounts);
            cache
                .pop(&kind, app_ctx.ndb, &mut scoped_subs)
                .expect("pop ok");
        }

        // --- Step 3: post B while the profile is closed ---
        publish_note(&mut h, "post B", 1_700_000_200);
        {
            let app_ctx = h.notedeck.app_context();
            wait_for_count(app_ctx.ndb, &author_filter, 2);
        }

        // --- Step 4: reopen profile and run the scheduled load again ---
        {
            let mut app_ctx = h.notedeck.app_context();
            let ndb = &mut *app_ctx.ndb;
            let txn = Transaction::new(ndb).expect("txn");
            let mut scoped_subs = app_ctx.remote.scoped_subs(app_ctx.accounts);
            cache.open(
                ndb,
                app_ctx.note_cache,
                &txn,
                &mut scoped_subs,
                &kind,
                pk,
                false,
                remote_policy(),
            );
        }
        {
            let app_ctx = h.notedeck.app_context();
            run_scheduled_initial_load(
                &mut cache,
                &kind,
                app_ctx.ndb,
                app_ctx.note_cache,
                app_ctx.unknown_ids,
            );
        }

        // --- Step 5: post B must now be visible ---
        let view_len = cache
            .get(&kind)
            .expect("timeline")
            .current_view()
            .units
            .len();
        assert_eq!(
            view_len, 2,
            "after reopening, both posts A and B should be visible (got {view_len})"
        );
    }
}
