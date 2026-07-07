use hashbrown::{HashMap as HbHashMap, HashSet};
use negentropy::{Id, Negentropy, NegentropyStorageVector};
use nostrdb::Filter;
use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use super::full_history::state::{
    PendingIngestion, TrackedFullHistorySub, FULL_HISTORY_RETRY_BACKOFF_BASE, INGESTION_TIMEOUT,
    MAX_FULL_HISTORY_FETCH_RETRIES_PER_RELAY_ID, MAX_FULL_HISTORY_RETRIES_PER_RELAY_FILTER,
};
use super::full_history::{
    FullHistoryFetchRequest, FullHistoryNeed, FullHistoryOutput, FullHistorySnapshot,
};
use super::service::full_history_runtime::FullHistoryRuntimeOutput;
use super::*;
use crate::relay::negentropy::ActiveSessionRelayDemand;
use crate::relay::negentropy::NegentropyStartResult;
use crate::relay::{
    normalize_full_history_targets,
    subscription::{FullHistoryTask, FullHistoryUpsertTask},
    test_utils::{create_text_capture_relay, filters_json, trivial_filter, MockWakeup, Wakeup},
    FullHistoryConfig, FullHistoryRelayFilter, FullHistoryTarget, Nip11ApplyOutcome,
    RelayDemandPriority, RelayLimitations, RelayRoutingPreference, RelayUrlPkgs, RelayUrlSource,
    SubPassGuardian,
};
use crate::test_support::outbox::{test_outbox_service, TestOutboxService};
use crate::NoteId;

const NEG_OPEN_PREFIX: &str = r#"["NEG-OPEN","#;
const NEG_CLOSE_PREFIX: &str = r#"["NEG-CLOSE","#;
const MIN_NEGENTROPY_FRAME_SIZE_LIMIT: u64 = 4097;

#[derive(Clone, Default)]
struct ProgressWakeup {
    woke: Arc<AtomicBool>,
}

impl ProgressWakeup {
    fn woke(&self) -> bool {
        self.woke.load(AtomicOrdering::Relaxed)
    }
}

impl Wakeup for ProgressWakeup {
    fn wake(&self) {
        self.woke.store(true, AtomicOrdering::Relaxed);
    }
}

struct OutboxPool {
    service: TestOutboxService,
    local_set_requests: Vec<FullHistoryLocalSetRequest>,
    local_presence_requests: Vec<FullHistoryLocalPresenceRequest>,
    pending_ingestion_presence_requests: Vec<FullHistoryPendingIngestionPresenceRequest>,
}

impl Default for OutboxPool {
    fn default() -> Self {
        Self {
            service: test_outbox_service(),
            local_set_requests: Vec::new(),
            local_presence_requests: Vec::new(),
            pending_ingestion_presence_requests: Vec::new(),
        }
    }
}

impl Deref for OutboxPool {
    type Target = super::OutboxPool;

    fn deref(&self) -> &Self::Target {
        &self.service.pool
    }
}

impl DerefMut for OutboxPool {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.service.pool
    }
}

impl OutboxPool {
    fn record_full_history_output(
        &mut self,
        output: FullHistoryOutput,
        commands: &mut TestOutboxCommands,
    ) {
        self.local_set_requests.extend(output.local_set_requests);
        self.local_presence_requests
            .extend(output.local_presence_requests);
        self.pending_ingestion_presence_requests
            .extend(output.pending_ingestion_presence_requests);
        for request in output.fetch_requests {
            commands.insert_full_history_fetch(self.service.pool.next_sub_id(), request);
        }
    }

    fn record_pool_full_history_effects(
        &mut self,
        output: OutboxPoolOutput,
        commands: &mut TestOutboxCommands,
    ) {
        for effect in output.full_history_effects {
            match effect {
                OutboxFullHistoryEffect::NegentropyCapacityGranted { relay, grant } => {
                    let output = self
                        .service
                        .apply_full_history_negentropy_capacity_grant(relay, grant);
                    self.record_full_history_runtime_output(output, commands);
                }
                OutboxFullHistoryEffect::NegentropyEffect { relay, effect } => {
                    let output = self.service.apply_negentropy_effect(&relay, effect);
                    self.record_full_history_runtime_output(output, commands);
                }
            }
        }
    }

    fn record_full_history_runtime_output(
        &mut self,
        output: FullHistoryRuntimeOutput,
        commands: &mut TestOutboxCommands,
    ) {
        self.record_pool_full_history_effects(output.pool, commands);
        self.record_full_history_output(output.full_history, commands);
    }

    fn record_full_history_local_output(&mut self, output: FullHistoryOutput) {
        self.local_set_requests.extend(output.local_set_requests);
        self.local_presence_requests
            .extend(output.local_presence_requests);
        self.pending_ingestion_presence_requests
            .extend(output.pending_ingestion_presence_requests);
        assert!(
            output.fetch_requests.is_empty(),
            "full-history fetch output needs an explicit command sink"
        );
    }

    fn poll_full_history(&mut self, commands: &mut TestOutboxCommands) -> bool {
        let output = self
            .service
            .apply_full_history_workflow_deadline_due(Instant::now());
        let has_pool_work = !pool_output_is_empty(&output.pool);
        self.record_full_history_runtime_output(output, commands);
        has_pool_work
    }

    fn poll_full_history_deadline(&mut self, commands: &mut TestOutboxCommands) {
        let output = self
            .service
            .apply_full_history_workflow_deadline_due(Instant::now());
        self.record_full_history_runtime_output(output, commands);
    }

    fn poll_full_history_deadline_at(&mut self, now: Instant, commands: &mut TestOutboxCommands) {
        let output = self.service.apply_full_history_workflow_deadline_due(now);
        self.record_full_history_runtime_output(output, commands);
    }

    fn poll_negentropy_state_machine(&mut self) -> bool {
        let mut commands = TestOutboxCommands::default();
        let output = self
            .service
            .apply_full_history_workflow_deadline_due(Instant::now());
        let has_pool_work = !pool_output_is_empty(&output.pool);
        self.record_full_history_runtime_output(output, &mut commands);
        has_pool_work || !commands.full_history_fetches.is_empty()
    }

    fn apply_unsupported_subid_length(
        &mut self,
        relay: &NormRelayUrl,
        max_subid_length: usize,
    ) -> Nip11ApplyOutcome {
        let (outcome, _output) = self
            .service
            .apply_unsupported_subid_length(relay, max_subid_length);
        outcome
    }

    fn apply_relay_limit_update(
        &mut self,
        relay: &NormRelayUrl,
        limitations: RelayLimitations,
    ) -> Nip11ApplyOutcome {
        let (outcome, _output) = self.service.apply_relay_limit_update(relay, limitations);
        outcome
    }

    fn has_full_history_work(&self) -> bool {
        self.service.full_history.has_pending_work()
            || self.service.pool.relays.iter().any(|(relay_id, relay)| {
                relay.supports_relay_subscription_ids()
                    && self.service.negentropy.has_work(relay_id)
            })
    }

    fn next_deadline(&self) -> Option<Instant> {
        [
            self.service.full_history.next_deadline(Instant::now()),
            self.service
                .pool
                .relays
                .iter()
                .filter_map(|(relay_id, relay)| {
                    relay
                        .supports_relay_subscription_ids()
                        .then(|| self.service.negentropy.next_timeout_deadline(relay_id))
                        .flatten()
                })
                .min(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn full_history_snapshot(&self, id: FullHistorySubId) -> Option<FullHistorySnapshot> {
        self.service.full_history.full_history_snapshot(id)
    }

    fn full_history_catchup_complete(&self, id: FullHistorySubId) -> bool {
        self.service.full_history.full_history_catchup_complete(
            &self.service.pool,
            &self.service.negentropy,
            id,
        )
    }

    fn apply_full_history_local_set_ready(
        &mut self,
        history_id: FullHistorySubId,
        request_id: u64,
        storage: NegentropyStorageVector,
    ) -> bool {
        let (applied, output) = self
            .service
            .apply_full_history_local_set_ready(history_id, request_id, storage);
        let mut commands = TestOutboxCommands::default();
        self.record_full_history_runtime_output(output, &mut commands);
        applied
    }

    fn apply_full_history_local_set_failed(
        &mut self,
        history_id: FullHistorySubId,
        request_id: u64,
    ) -> bool {
        let (applied, output) = self
            .service
            .apply_full_history_local_set_failed(history_id, request_id);
        let mut commands = TestOutboxCommands::default();
        self.record_full_history_runtime_output(output, &mut commands);
        applied
    }

    fn apply_pending_ingestion_presence_result(
        &mut self,
        result: FullHistoryPendingIngestionPresenceResult,
    ) -> Vec<FullHistorySubId> {
        let (completed, output) = self.service.apply_pending_ingestion_presence_result(result);
        let mut commands = TestOutboxCommands::default();
        self.record_full_history_runtime_output(output, &mut commands);
        completed
    }

    fn relay_transport_demand(&self, relay_id: &NormRelayUrl) -> Option<RelayTransportDemand> {
        let mut demand = self.service.pool.relay_transport_demand(relay_id);
        self.service.full_history.for_each_relay_transport_demand(
            |relay, priority, source, connection_weight| {
                if self.service.pool.relay_subscription_ids_unsupported(relay) {
                    return;
                }
                if relay == relay_id {
                    demand = RelayTransportDemand::merge_optional(
                        demand,
                        Some(RelayTransportDemand::new(
                            priority,
                            source,
                            connection_weight,
                        )),
                    );
                }
            },
        );
        demand
    }

    fn relay_connection_priority(
        &self,
        relay_id: &NormRelayUrl,
    ) -> Option<RelayConnectionPriority> {
        self.relay_transport_demand(relay_id)
            .map(|demand| demand.priority)
    }
}

fn drive_transport_once(pool: &mut OutboxPool, _wakeup: &MockWakeup) {
    receive_active_relays(pool);
}

fn receive_active_relays(pool: &mut OutboxPool) {
    let _ = pool;
}

fn apply_send_session_with<W: Wakeup, T>(
    pool: &mut OutboxPool,
    wakeup: W,
    f: impl FnOnce(&mut OutboxPool, &mut TestOutboxCommands) -> T,
) -> T {
    let mut tasks = TestOutboxCommands::default();
    let output = f(pool, &mut tasks);
    apply_test_outbox_commands(pool, tasks, &wakeup);
    output
}

#[derive(Default)]
struct TestOutboxCommands {
    live_output: OutboxPoolOutput,
    full_history_tasks: HbHashMap<FullHistorySubId, FullHistoryTask>,
    full_history_fetches: HbHashMap<OutboxSubId, FullHistoryFetchRequest>,
}

impl TestOutboxCommands {
    fn insert_full_history_fetch(&mut self, id: OutboxSubId, request: FullHistoryFetchRequest) {
        self.full_history_fetches.insert(id, request);
    }

    fn is_empty(&self) -> bool {
        pool_output_is_empty(&self.live_output)
            && self.full_history_tasks.is_empty()
            && self.full_history_fetches.is_empty()
    }

    fn subscribe(
        &mut self,
        pool: &mut OutboxPool,
        id: OutboxSubId,
        filters: Vec<Filter>,
        relays: RelayUrlPkgs,
    ) {
        self.live_output.extend(pool.set_live(id, filters, relays));
    }

    fn oneshot(
        &mut self,
        pool: &mut OutboxPool,
        id: OutboxSubId,
        filters: Vec<Filter>,
        relays: RelayUrlPkgs,
    ) -> bool {
        let accepted = filters.iter().any(|filter| filter.num_elements() != 0);
        self.live_output
            .extend(pool.start_fetch(id, filters, relays));
        accepted
    }

    fn new_filters(&mut self, pool: &mut OutboxPool, id: OutboxSubId, filters: Vec<Filter>) {
        let Some(sub) = pool.subs.get(&id) else {
            return;
        };
        let relays = relay_pkgs_from_sub(sub, sub.relays.clone());
        self.live_output.extend(pool.set_live(id, filters, relays));
    }

    fn modify_full(
        &mut self,
        pool: &mut OutboxPool,
        id: OutboxSubId,
        filters: Vec<Filter>,
        relays: HashSet<NormRelayUrl>,
    ) {
        let Some(sub) = pool.subs.get(&id) else {
            return;
        };
        let relays = relay_pkgs_from_sub(sub, relays);
        self.live_output.extend(pool.set_live(id, filters, relays));
    }

    fn upsert_full_history(
        &mut self,
        id: FullHistorySubId,
        full_history: FullHistoryConfig,
        relay_pkgs: Vec<RelayUrlPkgs>,
    ) -> bool {
        self.upsert_full_history_targets(
            id,
            vec![FullHistoryTarget::new(full_history.filters, relay_pkgs)],
        )
    }

    fn upsert_full_history_targets(
        &mut self,
        id: FullHistorySubId,
        targets: Vec<FullHistoryTarget>,
    ) -> bool {
        let targets = normalize_full_history_targets(targets);
        if targets.is_empty() {
            self.remove_full_history(id);
            return false;
        }

        self.full_history_tasks.insert(
            id,
            FullHistoryTask::Upsert(FullHistoryUpsertTask { targets }),
        );
        true
    }

    fn remove_full_history(&mut self, id: FullHistorySubId) {
        self.full_history_tasks.insert(id, FullHistoryTask::Remove);
    }
}

fn apply_test_outbox_commands<W: Wakeup>(
    pool: &mut OutboxPool,
    mut tasks: TestOutboxCommands,
    wakeup: &W,
) {
    let progress = ProgressWakeup::default();
    let pool_output = std::mem::take(&mut tasks.live_output);
    let (full_history_eviction_candidates, full_history_output) = pool
        .service
        .apply_full_history_tasks(std::mem::take(&mut tasks.full_history_tasks));
    let _ = full_history_eviction_candidates;
    let _ = pool_output;
    pool.record_full_history_runtime_output(full_history_output, &mut tasks);

    if pool.poll_full_history(&mut tasks) {
        progress.wake();
    }
    if !tasks.full_history_fetches.is_empty() {
        let followup_pool_output = pool.service.pool.start_full_history_fetches(
            std::mem::take(&mut tasks.full_history_fetches)
                .into_iter()
                .collect(),
        );
        let _ = followup_pool_output;
    }

    if progress.woke() {
        wakeup.wake();
    }
}

fn collect_test_command_batch(
    pool: &mut OutboxPool,
    mut commands: TestOutboxCommands,
) -> OutboxPoolOutput {
    let mut output = std::mem::take(&mut commands.live_output);
    output.extend(
        pool.start_full_history_fetches(
            std::mem::take(&mut commands.full_history_fetches)
                .into_iter()
                .collect(),
        ),
    );
    output
}

fn output_touches_relay(output: &OutboxPoolOutput, relay: &NormRelayUrl) -> bool {
    output.relay_demand_changes.iter().any(|change| &change.relay == relay)
        || output.transport_effects.iter().any(|effect| {
            matches!(effect, OutboxTransportEffect::SendRelayFrame { relay: effect_relay, .. } if effect_relay == relay)
        })
        || output.facts.iter().any(|fact| {
            matches!(fact, OutboxPoolFact::RelayReqStatus { relay: fact_relay, .. } if fact_relay == relay)
        })
}

fn pool_output_is_empty(output: &OutboxPoolOutput) -> bool {
    output.facts.is_empty()
        && output.relay_demand_changes.is_empty()
        && output.transport_effects.is_empty()
        && output.full_history_effects.is_empty()
}

fn relay_pkgs_from_sub(sub: &OutboxSubscription, relays: HashSet<NormRelayUrl>) -> RelayUrlPkgs {
    RelayUrlPkgs::new(
        relays,
        crate::relay::RelayUrlPolicy::new(
            sub.relay_url_source,
            sub.demand_priority,
            sub.routing_preference,
        )
        .with_connection_weight(sub.connection_weight),
    )
}

fn subscribe(
    pool: &mut OutboxPool,
    session: &mut TestOutboxCommands,
    filters: Vec<Filter>,
    urls: RelayUrlPkgs,
) -> OutboxSubId {
    let id = pool.next_sub_id();
    session.subscribe(pool, id, filters, urls);
    id
}

fn upsert_full_history(
    pool: &mut OutboxPool,
    session: &mut TestOutboxCommands,
    full_history: FullHistoryConfig,
    relay_pkgs: Vec<RelayUrlPkgs>,
) -> FullHistorySubId {
    let id = pool.id_registry.next_full_history_id();
    assert!(session.upsert_full_history(id, full_history, relay_pkgs));
    id
}

fn update_full_history(
    session: &mut TestOutboxCommands,
    id: FullHistorySubId,
    full_history: FullHistoryConfig,
    relay_pkgs: Vec<RelayUrlPkgs>,
) -> bool {
    session.upsert_full_history(id, full_history, relay_pkgs)
}

fn upsert_full_history_targets(
    pool: &mut OutboxPool,
    session: &mut TestOutboxCommands,
    targets: Vec<FullHistoryTarget>,
) -> FullHistorySubId {
    let id = pool.id_registry.next_full_history_id();
    assert!(session.upsert_full_history_targets(id, targets));
    id
}

fn update_full_history_targets(
    session: &mut TestOutboxCommands,
    id: FullHistorySubId,
    targets: Vec<FullHistoryTarget>,
) -> bool {
    session.upsert_full_history_targets(id, targets)
}

fn modify_filters(
    pool: &mut OutboxPool,
    session: &mut TestOutboxCommands,
    id: OutboxSubId,
    filters: Vec<Filter>,
) {
    session.new_filters(pool, id, filters);
}

fn modify_full(
    pool: &mut OutboxPool,
    session: &mut TestOutboxCommands,
    id: OutboxSubId,
    filters: Vec<Filter>,
    relays: HashSet<NormRelayUrl>,
) {
    session.modify_full(pool, id, filters, relays);
}

fn force_full_history_retries_due(pool: &mut OutboxPool, history_id: FullHistorySubId) {
    let Some(tracked) = pool.service.full_history.tracked_subs.get_mut(&history_id) else {
        return;
    };
    for retry in &mut tracked.progress.retry_states {
        if retry.next_retry_at.is_some() {
            retry.next_retry_at = Some(Instant::now());
        }
    }
}

fn ready_pool() -> OutboxPool {
    OutboxPool::default()
}

fn counting_ready_pool(calls: Arc<AtomicUsize>) -> OutboxPool {
    calls.fetch_add(0, AtomicOrdering::SeqCst);
    OutboxPool::default()
}

fn take_local_presence_requests(pool: &mut OutboxPool) -> Vec<FullHistoryLocalPresenceRequest> {
    std::mem::take(&mut pool.local_presence_requests)
}

fn take_pending_ingestion_presence_requests(
    pool: &mut OutboxPool,
) -> Vec<FullHistoryPendingIngestionPresenceRequest> {
    std::mem::take(&mut pool.pending_ingestion_presence_requests)
}

fn take_local_set_requests(pool: &mut OutboxPool) -> Vec<FullHistoryLocalSetRequest> {
    std::mem::take(&mut pool.local_set_requests)
}

fn apply_local_presence_requests_into(
    pool: &mut OutboxPool,
    present: &HashSet<NoteId>,
    commands: &mut TestOutboxCommands,
) -> usize {
    let requests = take_local_presence_requests(pool);
    let count = requests.len();
    for request in requests {
        let mut missing_ids = HashSet::new();
        let mut already_local_ids = HashSet::new();
        for id in request.candidate_ids {
            if present.contains(&id) {
                already_local_ids.insert(id);
            } else {
                missing_ids.insert(id);
            }
        }
        let (applied, output) =
            pool.service
                .apply_full_history_local_presence_ready(FullHistoryLocalPresenceResult {
                    request_id: request.request_id,
                    missing_ids,
                    already_local_ids,
                });
        if applied {
            pool.record_full_history_runtime_output(output, commands);
        }
    }
    count
}

fn empty_negentropy_storage() -> NegentropyStorageVector {
    let mut storage = NegentropyStorageVector::new();
    storage.seal().expect("test storage should seal");
    storage
}

fn negentropy_storage_with_notes(ids: impl IntoIterator<Item = NoteId>) -> NegentropyStorageVector {
    let mut storage = NegentropyStorageVector::new();
    for (index, id) in ids.into_iter().enumerate() {
        storage
            .insert(index as u64, Id::from_byte_array(*id.bytes()))
            .expect("insert test negentropy id");
    }
    storage.seal().expect("test storage should seal");
    storage
}

fn surface_relay_negentropy_need(
    pool: &mut OutboxPool,
    relay: &NormRelayUrl,
    history_id: FullHistorySubId,
    id: NoteId,
) {
    let filter = trivial_filter()[0].clone();
    let mut guardian = SubPassGuardian::new(1);
    let pass = guardian.take_pass().expect("test subpass");
    let start_msg = match pool.service.negentropy.try_start_full_history(
        relay,
        pass,
        empty_negentropy_storage,
        filter,
        history_id,
        ActiveSessionRelayDemand::single(RelayDemandPriority::Important, 0),
    ) {
        NegentropyStartResult::Started(msg) => msg,
        NegentropyStartResult::Rejected(_) => panic!("test negentropy session should start"),
    };

    let start_json = start_msg.to_json().expect("serialize NEG-OPEN");
    let start_value: serde_json::Value = serde_json::from_str(&start_json).expect("parse NEG-OPEN");
    let start_array = start_value.as_array().expect("NEG-OPEN array");
    let session_id = start_array[1].as_str().expect("NEG-OPEN session id");
    let init_hex = start_array[3].as_str().expect("NEG-OPEN init payload");
    let init_msg = hex::decode(init_hex).expect("decode NEG-OPEN payload");

    let relay_storage = negentropy_storage_with_notes([id]);
    let mut relay_neg = Negentropy::borrowed(&relay_storage, MIN_NEGENTROPY_FRAME_SIZE_LIMIT)
        .expect("relay negentropy");
    let relay_msg = relay_neg.reconcile(&init_msg).expect("relay NEG-MSG");

    let _ = pool
        .service
        .apply_relay_neg_msg(relay, 0, session_id, &hex::encode(relay_msg));
}

fn apply_ready_local_set_requests(pool: &mut OutboxPool) -> usize {
    let mut commands = TestOutboxCommands::default();
    apply_ready_local_set_requests_into(pool, &mut commands)
}

fn apply_ready_local_set_requests_into(
    pool: &mut OutboxPool,
    commands: &mut TestOutboxCommands,
) -> usize {
    let requests = take_local_set_requests(pool);
    let count = requests.len();
    for request in requests {
        let (applied, output) = pool.service.apply_full_history_local_set_ready(
            request.history_id,
            request.request_id,
            empty_negentropy_storage(),
        );
        if applied {
            pool.record_full_history_runtime_output(output, commands);
        }
    }
    count
}

fn apply_ready_local_set_requests_counted(pool: &mut OutboxPool, calls: &AtomicUsize) -> usize {
    let mut commands = TestOutboxCommands::default();
    apply_ready_local_set_requests_counted_into(pool, &mut commands, calls)
}

fn apply_ready_local_set_requests_counted_into(
    pool: &mut OutboxPool,
    commands: &mut TestOutboxCommands,
    calls: &AtomicUsize,
) -> usize {
    let count = apply_ready_local_set_requests_into(pool, commands);
    calls.fetch_add(count, AtomicOrdering::SeqCst);
    count
}

fn poll_full_history_with_ready_local_sets(
    pool: &mut OutboxPool,
    session: &mut TestOutboxCommands,
) -> bool {
    let mut has_pool_work = pool.poll_full_history(session);
    if apply_ready_local_set_requests_into(pool, session) > 0 {
        has_pool_work |= pool.poll_full_history(session);
    }
    if apply_local_presence_requests_into(pool, &HashSet::new(), session) > 0 {
        has_pool_work |= pool.poll_full_history(session);
    }
    has_pool_work
}

fn poll_full_history_with_counted_ready_local_sets(
    pool: &mut OutboxPool,
    session: &mut TestOutboxCommands,
    calls: &AtomicUsize,
) -> bool {
    let mut has_pool_work = pool.poll_full_history(session);
    if apply_ready_local_set_requests_counted_into(pool, session, calls) > 0 {
        has_pool_work |= pool.poll_full_history(session);
    }
    if apply_local_presence_requests_into(pool, &HashSet::new(), session) > 0 {
        has_pool_work |= pool.poll_full_history(session);
    }
    has_pool_work
}

fn poll_negentropy_state_machine_with_ready_local_sets(pool: &mut OutboxPool) -> bool {
    apply_ready_local_set_requests(pool);
    pool.poll_negentropy_state_machine()
}

fn note_id(byte: u8) -> NoteId {
    NoteId::new([byte; 32])
}

fn relay_url(name: &str) -> NormRelayUrl {
    NormRelayUrl::new(&format!("wss://relay-full-history-{name}.invalid")).unwrap()
}

fn relay_filter_target(relay: NormRelayUrl) -> FullHistoryRelayFilter {
    FullHistoryRelayFilter {
        relay_policy: test_relay_pkgs(HashSet::from([relay.clone()])).policy(),
        relay,
        filter: trivial_filter()[0].clone(),
    }
}

fn filter_larger_than_default_json_buffer() -> Filter {
    let mut ids = Vec::new();
    for index in 0..18_000u64 {
        let mut id = [0u8; 32];
        id[..8].copy_from_slice(&index.to_be_bytes());
        ids.push(id);
    }
    let filter = Filter::new_with_capacity(512).ids(ids.iter()).build();
    assert!(
        filter.json().is_err(),
        "test filter should exceed Filter::json default buffer"
    );
    filter
}

fn pending_ingestion(relay: NormRelayUrl, started_at: Instant) -> PendingIngestion {
    PendingIngestion {
        target: relay_filter_target(relay),
        started_at,
        retries_started: 0,
    }
}

fn after_ingestion_timeout() -> Instant {
    Instant::now() + INGESTION_TIMEOUT + Duration::from_millis(1)
}

fn after_ingestion_timeout_and_fetch_retry_backoff() -> Instant {
    let max_fetch_retry_delay = FULL_HISTORY_RETRY_BACKOFF_BASE
        .saturating_mul(1 << MAX_FULL_HISTORY_FETCH_RETRIES_PER_RELAY_ID);
    Instant::now() + INGESTION_TIMEOUT + max_fetch_retry_delay + Duration::from_millis(2)
}

fn seed_fetch_retry(
    tracked: &mut TrackedFullHistorySub,
    id: NoteId,
    relay: NormRelayUrl,
    next_retry_at: Instant,
) {
    tracked
        .progress
        .upsert_fetch_retry(id, relay_filter_target(relay), 1, next_retry_at);
}

fn full_history_need(
    history_id: FullHistorySubId,
    relay: NormRelayUrl,
    id: NoteId,
) -> FullHistoryNeed {
    FullHistoryNeed {
        history_id,
        target: relay_filter_target(relay),
        id,
    }
}

fn tracked_sub(pool: &OutboxPool, history_id: FullHistorySubId) -> &TrackedFullHistorySub {
    pool.service
        .full_history
        .tracked_subs
        .get(&history_id)
        .expect("full-history sub should be tracked")
}

fn tracked_sub_mut(
    pool: &mut OutboxPool,
    history_id: FullHistorySubId,
) -> &mut TrackedFullHistorySub {
    pool.service
        .full_history
        .tracked_subs
        .get_mut(&history_id)
        .expect("full-history sub should be tracked")
}

fn tracked_snapshot_relays(tracked: &TrackedFullHistorySub) -> Vec<NormRelayUrl> {
    let mut relays = tracked
        .snapshot
        .relay_filters
        .iter()
        .map(|relay_filter| relay_filter.relay.clone())
        .collect::<Vec<_>>();
    relays.sort_by_key(|relay| relay.to_string());
    relays.dedup();
    relays
}

fn queued_need_id_count(tracked: &TrackedFullHistorySub) -> usize {
    tracked
        .progress
        .pending_needs
        .iter()
        .map(|needs| needs.ids.len())
        .sum()
}

fn is_tracked(pool: &OutboxPool, history_id: FullHistorySubId) -> bool {
    pool.service
        .full_history
        .tracked_subs
        .contains_key(&history_id)
}

fn relay_data<'a>(pool: &'a OutboxPool, relay: &NormRelayUrl) -> &'a CoordinationData {
    pool.relays.get(relay).expect("relay tracked")
}

fn clear_pending_neg_sets(pool: &mut OutboxPool, history_id: FullHistorySubId) {
    let progress = &mut tracked_sub_mut(pool, history_id).progress;
    progress.pending_neg_sets.clear();
}

fn pending_neg_set_relays(
    pool: &OutboxPool,
    history_id: FullHistorySubId,
) -> HashSet<NormRelayUrl> {
    tracked_sub(pool, history_id)
        .progress
        .pending_neg_sets
        .iter()
        .flat_map(|pending| pending.relays.iter().cloned())
        .collect()
}

fn stage_need_fetches_for_test(
    pool: &mut OutboxPool,
    needs: Vec<FullHistoryNeed>,
    session: &mut TestOutboxCommands,
    present: &HashSet<NoteId>,
) -> Vec<FullHistorySubId> {
    let (output, initial) = pool
        .service
        .full_history
        .stage_need_fetches(needs, Instant::now());
    pool.record_full_history_output(output, session);
    assert!(initial.is_empty());
    apply_local_presence_requests_into(pool, present, session);
    let (output, ready) = pool
        .service
        .full_history
        .stage_need_fetches(Vec::new(), Instant::now());
    pool.record_full_history_output(output, session);
    ready
}

fn queue_snapshot_need(
    pool: &mut OutboxPool,
    history_id: FullHistorySubId,
    relay: &NormRelayUrl,
    filter: &Filter,
    id: NoteId,
) {
    let target = tracked_sub(pool, history_id)
        .snapshot
        .target_for_relay_filter(relay, filter)
        .expect("snapshot should contain queued need target");
    pool.service.full_history.queue_needs(vec![FullHistoryNeed {
        history_id,
        target,
        id,
    }]);
}

fn seed_relay_need(
    pool: &mut OutboxPool,
    relay: &NormRelayUrl,
    history_id: FullHistorySubId,
    id: NoteId,
) {
    queue_snapshot_need(pool, history_id, relay, &trivial_filter()[0], id);
}

fn seed_relay_retry(pool: &mut OutboxPool, relay: &NormRelayUrl, history_id: FullHistorySubId) {
    let filter = trivial_filter()[0].clone();
    let target = tracked_sub(pool, history_id)
        .snapshot
        .target_for_relay_filter(relay, &filter)
        .expect("retry seed target should exist");
    tracked_sub_mut(pool, history_id)
        .progress
        .schedule_retry(target, Instant::now());
}

fn assert_full_history_retry_scheduled(
    pool: &OutboxPool,
    history_id: FullHistorySubId,
    relay: &NormRelayUrl,
    after: Instant,
) {
    let tracked = tracked_sub(pool, history_id);
    assert_eq!(tracked.progress.retry_states.len(), 1);
    let retry = &tracked.progress.retry_states[0];
    assert_eq!(&retry.target.relay, relay);
    assert!(retry
        .target
        .filter
        .same_canonical_attributes(&trivial_filter()[0]));
    assert_eq!(retry.attempts_started, 0);
    let next_retry_at = retry.next_retry_at.expect("retry should be scheduled");
    assert!(next_retry_at > Instant::now());
    assert!(next_retry_at <= after + FULL_HISTORY_RETRY_BACKOFF_BASE + Duration::from_millis(100));
}

fn assert_active_sessions(pool: &OutboxPool, relay: &NormRelayUrl, count: usize) {
    assert_eq!(
        pool.service
            .negentropy
            .relay(relay)
            .map(|data| data.active_session_count())
            .unwrap_or_default(),
        count
    );
}

fn active_negentropy_session_id(pool: &OutboxPool, relay: &NormRelayUrl) -> String {
    pool.service
        .negentropy
        .relay(relay)
        .and_then(|data| data.first_active_session_id_for_test())
        .expect("active negentropy session")
}

fn relay_set(relays: impl IntoIterator<Item = NormRelayUrl>) -> HashSet<NormRelayUrl> {
    relays.into_iter().collect()
}

fn test_relay_pkgs(relays: HashSet<NormRelayUrl>) -> RelayUrlPkgs {
    test_relay_pkgs_with_priority(relays, RelayDemandPriority::Important)
}

fn test_relay_pkgs_with_priority(
    relays: HashSet<NormRelayUrl>,
    priority: RelayDemandPriority,
) -> RelayUrlPkgs {
    RelayUrlPkgs::new(
        relays,
        crate::relay::RelayUrlPolicy::explicit(
            priority,
            crate::relay::RelayRoutingPreference::PreferDedicated,
        ),
    )
}

fn test_history_relay_pkgs(relays: HashSet<NormRelayUrl>) -> Vec<RelayUrlPkgs> {
    vec![test_relay_pkgs(relays)]
}

fn subscribe_with_history(
    pool: &mut OutboxPool,
    wakeup: impl Wakeup,
    filters: Vec<Filter>,
    history_filters: Vec<Filter>,
    relays: impl IntoIterator<Item = NormRelayUrl>,
) -> FullHistorySubId {
    apply_send_session_with(pool, wakeup, |pool, session| {
        let relays = relay_set(relays);
        subscribe(pool, session, filters, test_relay_pkgs(relays.clone()));
        upsert_full_history(
            pool,
            session,
            FullHistoryConfig::new(history_filters),
            test_history_relay_pkgs(relays),
        )
    })
}

fn subscribe_history_only(
    pool: &mut OutboxPool,
    wakeup: impl Wakeup,
    history_filters: Vec<Filter>,
    relays: impl IntoIterator<Item = NormRelayUrl>,
) -> FullHistorySubId {
    apply_send_session_with(pool, wakeup, |pool, session| {
        upsert_full_history(
            pool,
            session,
            FullHistoryConfig::new(history_filters),
            test_history_relay_pkgs(relay_set(relays)),
        )
    })
}

fn assert_relay_priority(
    pool: &mut OutboxPool,
    relay: &NormRelayUrl,
    expected: RelayDemandPriority,
) {
    let priority = pool
        .relay_connection_priority(relay)
        .expect("relay should have connection priority");
    assert_eq!(priority.strongest_demand, expected);
}

fn subscribe_unbounded(
    pool: &mut OutboxPool,
    wakeup: impl Wakeup,
    relays: impl IntoIterator<Item = NormRelayUrl>,
) -> FullHistorySubId {
    let filters = trivial_filter();
    subscribe_with_history(pool, wakeup, filters.clone(), filters, relays)
}

fn counting_retry_fixture(
    relay_name: &str,
) -> (
    Arc<AtomicUsize>,
    OutboxPool,
    NormRelayUrl,
    FullHistorySubId,
    TestOutboxCommands,
) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut pool = counting_ready_pool(Arc::clone(&calls));
    let relay = relay_url(relay_name);
    let sub_id = subscribe_unbounded(&mut pool, MockWakeup::default(), [relay.clone()]);
    assert_eq!(apply_ready_local_set_requests_counted(&mut pool, &calls), 1);
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
    clear_pending_neg_sets(&mut pool, sub_id);
    (calls, pool, relay, sub_id, TestOutboxCommands::default())
}

fn modify_relays_for_history(
    pool: &mut OutboxPool,
    wakeup: MockWakeup,
    history_id: FullHistorySubId,
    relays: impl IntoIterator<Item = NormRelayUrl>,
) {
    apply_send_session_with(pool, wakeup, |_, session| {
        update_full_history(
            session,
            history_id,
            FullHistoryConfig::new(trivial_filter()),
            test_history_relay_pkgs(relay_set(relays)),
        );
    })
}

fn modify_unbounded_history(
    pool: &mut OutboxPool,
    wakeup: MockWakeup,
    history_id: FullHistorySubId,
    relays: impl IntoIterator<Item = NormRelayUrl>,
) {
    apply_send_session_with(pool, wakeup, |_, session| {
        update_full_history(
            session,
            history_id,
            FullHistoryConfig::new(trivial_filter()),
            test_history_relay_pkgs(relay_set(relays)),
        );
    })
}

fn remove_full_history(pool: &mut OutboxPool, wakeup: MockWakeup, history_id: FullHistorySubId) {
    apply_send_session_with(pool, wakeup, |_, session| {
        session.remove_full_history(history_id);
    })
}

fn full_history_fetch_ids_by_relay(
    session: &TestOutboxCommands,
) -> HashMap<NormRelayUrl, OutboxSubId> {
    session
        .full_history_fetches
        .iter()
        .map(|(id, fetch)| {
            let relay = fetch
                .subscribe
                .relays
                .urls
                .iter()
                .next()
                .expect("test fetch should target one relay")
                .clone();
            (relay, *id)
        })
        .collect()
}

fn captured_count(captured: &Arc<Mutex<Vec<String>>>, prefix: &str) -> usize {
    captured
        .lock()
        .expect("lock captured frames")
        .iter()
        .filter(|text| text.starts_with(prefix))
        .count()
}

#[tokio::test]
async fn subscribe_full_history_tracks_snapshot_and_schedules_round() {
    let mut pool = ready_pool();
    let relay = relay_url("track");

    let sub_id = subscribe_history_only(
        &mut pool,
        MockWakeup::default(),
        trivial_filter(),
        [relay.clone()],
    );

    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.rounds_started, 1);
    assert_eq!(tracked_snapshot_relays(tracked), vec![relay.clone()]);
    assert_eq!(
        filters_json(&tracked.snapshot.filters()),
        filters_json(&trivial_filter())
    );

    assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
    let pending = &tracked.progress.pending_neg_sets[0];
    assert_eq!(pending.relays, vec![relay]);
    assert_eq!(
        filters_json(std::slice::from_ref(&pending.filter)),
        filters_json(&trivial_filter())
    );
}

#[tokio::test]
async fn pending_full_history_work_preserves_declared_connection_priority() {
    let mut pool = ready_pool();
    let relay = relay_url("priority-pending");
    let relay_pkgs =
        test_relay_pkgs_with_priority(relay_set([relay.clone()]), RelayDemandPriority::Critical);
    let history_id = {
        apply_send_session_with(&mut pool, MockWakeup::default(), |pool, session| {
            upsert_full_history(
                pool,
                session,
                FullHistoryConfig::new(trivial_filter()),
                vec![relay_pkgs.clone()],
            )
        })
    };

    assert_relay_priority(&mut pool, &relay, RelayDemandPriority::Critical);

    clear_pending_neg_sets(&mut pool, history_id);
    pool.service.full_history.queue_needs(vec![FullHistoryNeed {
        history_id,
        target: FullHistoryRelayFilter {
            relay_policy: relay_pkgs.policy(),
            relay: relay.clone(),
            filter: trivial_filter()[0].clone(),
        },
        id: note_id(0x9a),
    }]);

    assert_relay_priority(&mut pool, &relay, RelayDemandPriority::Critical);
}

#[tokio::test]
async fn upsert_full_history_sub_resnapshots_modified_filters_and_relays() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay_a = relay_url("a");
    let relay_b = relay_url("b");

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay_a]);

    let updated_filters = vec![Filter::new().kinds(vec![7]).limit(3).build()];
    {
        apply_send_session_with(&mut pool, wakeup, |_, session| {
            update_full_history(
                session,
                sub_id,
                FullHistoryConfig::new(updated_filters.clone()),
                test_history_relay_pkgs(relay_set([relay_b.clone()])),
            );
        })
    }

    let tracked = tracked_sub(&pool, sub_id);
    let expected_filters = FullHistoryConfig::new(updated_filters.clone())
        .filters()
        .to_vec();
    assert_eq!(tracked.rounds_started, 1);
    assert_eq!(tracked_snapshot_relays(tracked), vec![relay_b.clone()]);
    assert_eq!(
        filters_json(&tracked.snapshot.filters()),
        filters_json(&expected_filters)
    );

    assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
    let pending = &tracked.progress.pending_neg_sets[0];
    assert_eq!(pending.relays, vec![relay_b]);
    assert_eq!(
        filters_json(std::slice::from_ref(&pending.filter)),
        filters_json(&expected_filters)
    );
}

#[tokio::test]
async fn caller_filter_update_retains_current_relay_filter_progress() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("filter-update-retains-overlap");
    let filter_a = Filter::new().kinds([1]).limit(10).build();
    let filter_b = Filter::new().kinds([2]).limit(10).build();
    let filter_c = Filter::new().kinds([3]).limit(10).build();

    let history_id = {
        apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
            upsert_full_history_targets(
                pool,
                session,
                vec![FullHistoryTarget::new(
                    vec![filter_a.clone(), filter_b],
                    test_history_relay_pkgs(relay_set([relay.clone()])),
                )],
            )
        })
    };

    let target_a = tracked_sub(&pool, history_id)
        .snapshot
        .target_for_relay_filter(&relay, &filter_a)
        .expect("initial snapshot should target filter A");
    pool.service.full_history.queue_needs(vec![FullHistoryNeed {
        history_id,
        target: target_a.clone(),
        id: note_id(0x47),
    }]);
    tracked_sub_mut(&mut pool, history_id)
        .progress
        .start_pending_ingestion(
            note_id(0x48),
            PendingIngestion {
                target: target_a,
                started_at: Instant::now(),
                retries_started: 0,
            },
        );

    {
        apply_send_session_with(&mut pool, wakeup, |_pool, session| {
            update_full_history_targets(
                session,
                history_id,
                vec![FullHistoryTarget::new(
                    vec![filter_a.clone(), filter_c.clone()],
                    test_history_relay_pkgs(relay_set([relay.clone()])),
                )],
            );
        })
    }
    let mut fetch_session = TestOutboxCommands::default();
    apply_local_presence_requests_into(&mut pool, &HashSet::new(), &mut fetch_session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut fetch_session);

    let tracked = tracked_sub(&pool, history_id);
    assert_eq!(
        tracked.rounds_started, 1,
        "caller filter update should start a fresh round for the new snapshot"
    );
    assert_eq!(
        queued_need_id_count(tracked),
        0,
        "queued needs may be drained into fetch planning during session ingest"
    );
    let retained_ingestion_ids = tracked
        .progress
        .pending_ingestions()
        .filter_map(|(id, pending)| {
            pending
                .target
                .filter
                .same_canonical_attributes(&filter_a)
                .then_some(*id)
        })
        .collect::<HashSet<_>>();
    assert_eq!(
        retained_ingestion_ids,
        HashSet::from([note_id(0x47), note_id(0x48)]),
        "unchanged relay/filter fetch wait state should remain current work"
    );
    assert_eq!(tracked.progress.pending_neg_sets.len(), 2);
    assert_eq!(
        filters_json(
            &tracked
                .progress
                .pending_neg_sets
                .iter()
                .map(|pending| pending.filter.clone())
                .collect::<Vec<_>>()
        ),
        filters_json(&[filter_a, filter_c]),
        "pending local-set work should retain unchanged filters and add new filters"
    );
}

#[tokio::test]
async fn subscribe_full_history_builds_one_local_set_per_filter_across_relays() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut pool = counting_ready_pool(Arc::clone(&calls));
    let relay_a = relay_url("count-a");
    let relay_b = relay_url("count-b");
    let sub_id = subscribe_history_only(
        &mut pool,
        MockWakeup::default(),
        trivial_filter(),
        [relay_a.clone(), relay_b.clone()],
    );

    assert_eq!(apply_ready_local_set_requests_counted(&mut pool, &calls), 1);
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
    assert_eq!(
        pending_neg_set_relays(&pool, sub_id),
        HashSet::from([relay_a, relay_b])
    );
}
#[tokio::test]
async fn full_history_pending_sets_deduplicate_added_relay_and_verification_round() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut pool = counting_ready_pool(Arc::clone(&calls));
    let wakeup = MockWakeup::default();
    let relay_a = relay_url("dedup-a");
    let relay_b = relay_url("dedup-b");

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay_a.clone()]);
    assert_eq!(apply_ready_local_set_requests_counted(&mut pool, &calls), 1);
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

    clear_pending_neg_sets(&mut pool, sub_id);

    modify_relays_for_history(
        &mut pool,
        wakeup,
        sub_id,
        [relay_a.clone(), relay_b.clone()],
    );
    assert_eq!(apply_ready_local_set_requests_counted(&mut pool, &calls), 1);
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);

    let output = pool.service.full_history.schedule_round(sub_id);
    pool.record_full_history_local_output(output);

    assert_eq!(
        calls.load(AtomicOrdering::SeqCst),
        2,
        "verification round should reuse pending local-set work for the same filter"
    );
    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
    assert_eq!(
        pending_neg_set_relays(&pool, sub_id),
        HashSet::from([relay_a, relay_b])
    );
}
#[tokio::test]
async fn full_history_snapshot_uses_explicit_history_filter() {
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let relay = relay_url("filter");

    let live_filter = Filter::new().kinds(vec![1]).limit(10).since(123).build();
    let history_filter = Filter::new().kinds(vec![1]).limit(10).build();
    let sub_id = subscribe_with_history(
        &mut pool,
        wakeup,
        vec![live_filter],
        vec![history_filter.clone()],
        [relay],
    );

    let snapshot = pool
        .full_history_snapshot(sub_id)
        .expect("full-history snapshot should exist");
    let filters = snapshot.filters();

    assert_eq!(filters.len(), 1);
    assert_eq!(filters[0].limit(), history_filter.limit());
    assert!(filters[0].since().is_none());
}

#[tokio::test]
async fn full_history_targets_preserve_relay_scoped_filters() {
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let relay_a = relay_url("target-a");
    let relay_b = relay_url("target-b");
    let author_a = [0xAA; 32];
    let author_b = [0xBB; 32];
    let filter_a = Filter::new().authors([&author_a]).kinds([1]).build();
    let filter_b = Filter::new().authors([&author_b]).kinds([1]).build();

    let sub_id = {
        apply_send_session_with(&mut pool, wakeup, |pool, session| {
            upsert_full_history_targets(
                pool,
                session,
                vec![
                    FullHistoryTarget::new(
                        vec![filter_a.clone()],
                        test_history_relay_pkgs(HashSet::from([relay_a.clone()])),
                    ),
                    FullHistoryTarget::new(
                        vec![filter_b.clone()],
                        test_history_relay_pkgs(HashSet::from([relay_b.clone()])),
                    ),
                ],
            )
        })
    };

    let snapshot = pool
        .full_history_snapshot(sub_id)
        .expect("targeted full-history snapshot");
    assert!(snapshot
        .target_for_relay_filter(&relay_a, &filter_a)
        .is_some());
    assert!(snapshot
        .target_for_relay_filter(&relay_a, &filter_b)
        .is_none());
    assert!(snapshot
        .target_for_relay_filter(&relay_b, &filter_b)
        .is_some());
    assert!(snapshot
        .target_for_relay_filter(&relay_b, &filter_a)
        .is_none());
}

#[tokio::test]
async fn full_history_targets_merge_duplicate_relay_filter_policy() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("target-duplicate-policy");
    let filter = Filter::new().kinds([1]).limit(10).build();
    let remote_pkg = RelayUrlPkgs::new(
        HashSet::from([relay.clone()]),
        crate::relay::RelayUrlPolicy::remote_advertised(
            crate::relay::RelayDemandPriority::Opportunistic,
            crate::relay::RelayRoutingPreference::NoPreference,
        ),
    );
    let explicit_pkg = RelayUrlPkgs::new(
        HashSet::from([relay.clone()]),
        crate::relay::RelayUrlPolicy::explicit(
            crate::relay::RelayDemandPriority::Critical,
            crate::relay::RelayRoutingPreference::RequireDedicated,
        ),
    );

    let sub_id = {
        apply_send_session_with(&mut pool, wakeup, |pool, session| {
            upsert_full_history_targets(
                pool,
                session,
                vec![
                    FullHistoryTarget::new(vec![filter.clone()], vec![remote_pkg]),
                    FullHistoryTarget::new(vec![filter.clone()], vec![explicit_pkg]),
                ],
            )
        })
    };

    let snapshot = pool
        .full_history_snapshot(sub_id)
        .expect("targeted full-history snapshot");
    let relay_filters = snapshot.relay_filters();
    assert_eq!(
        relay_filters.len(),
        1,
        "duplicate canonical relay/filter pairs should merge before snapshot storage"
    );
    let merged = relay_filters[0].relay_policy;
    assert_eq!(merged.source(), RelayUrlSource::Explicit);
    assert_eq!(merged.demand_priority(), RelayDemandPriority::Critical);
    assert_eq!(
        merged.routing_preference(),
        RelayRoutingPreference::RequireDedicated
    );

    let request_id = pool.id_registry.next_sub_id_value_for_test();
    queue_snapshot_need(&mut pool, sub_id, &relay, &filter, note_id(0x72));
    let mut staged_session = TestOutboxCommands::default();
    pool.poll_full_history(&mut staged_session);
    apply_local_presence_requests_into(&mut pool, &HashSet::new(), &mut staged_session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut staged_session);

    let task = staged_session
        .full_history_fetches
        .get(&OutboxSubId(request_id))
        .expect("merged duplicate relay/filter policy should stage a fetch");
    let fetch = task;
    assert_eq!(fetch.subscribe.relays.source(), RelayUrlSource::Explicit);
    assert_eq!(
        fetch.subscribe.relays.demand_priority(),
        RelayDemandPriority::Critical
    );
    assert_eq!(
        fetch.subscribe.relays.routing_preference(),
        RelayRoutingPreference::RequireDedicated
    );
}

#[tokio::test]
async fn upsert_full_history_sub_preserves_progress_when_history_snapshot_is_unchanged() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("equivalent");

    let sub_id = subscribe_with_history(
        &mut pool,
        wakeup.clone(),
        vec![Filter::new().kinds(vec![1]).limit(10).since(123).build()],
        vec![Filter::new().kinds(vec![1]).build()],
        [relay.clone()],
    );

    let missing_id = note_id(0x44);
    {
        let tracked = tracked_sub_mut(&mut pool, sub_id);
        tracked.rounds_started = 7;
        tracked.progress.pending_neg_sets.clear();
        tracked
            .progress
            .start_pending_ingestion(missing_id, pending_ingestion(relay.clone(), Instant::now()));
    }

    {
        apply_send_session_with(&mut pool, wakeup, |_, session| {
            update_full_history(
                session,
                sub_id,
                FullHistoryConfig::new(vec![Filter::new().kinds(vec![1]).build()]),
                test_history_relay_pkgs(relay_set([relay.clone()])),
            );
        })
    }

    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.rounds_started, 7);
    assert!(
        tracked.progress.pending_neg_sets.is_empty(),
        "equivalent history snapshot should not enqueue a fresh round"
    );
    assert!(
        tracked.progress.pending_ingestion(&missing_id).is_some(),
        "equivalent history snapshot should preserve in-flight fetch tracking"
    );
}

#[tokio::test]
async fn upsert_full_history_sub_refreshes_policy_without_new_history_work() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("policy-refresh");
    let filter = Filter::new().kinds([1]).limit(10).build();
    let initial_pkg = RelayUrlPkgs::new(
        HashSet::from([relay.clone()]),
        crate::relay::RelayUrlPolicy::remote_advertised(
            RelayDemandPriority::Opportunistic,
            crate::relay::RelayRoutingPreference::NoPreference,
        ),
    );
    let updated_pkg = RelayUrlPkgs::new(
        HashSet::from([relay.clone()]),
        crate::relay::RelayUrlPolicy::explicit(
            RelayDemandPriority::Critical,
            crate::relay::RelayRoutingPreference::RequireDedicated,
        ),
    );
    let updated_target = FullHistoryTarget::new(vec![filter.clone()], vec![updated_pkg.clone()]);

    let sub_id = {
        apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
            upsert_full_history_targets(
                pool,
                session,
                vec![FullHistoryTarget::new(
                    vec![filter.clone()],
                    vec![initial_pkg.clone()],
                )],
            )
        })
    };
    {
        let tracked = tracked_sub_mut(&mut pool, sub_id);
        tracked.rounds_started = 7;
        tracked.progress.pending_neg_sets.clear();
    }

    let missing_id = note_id(0x45);
    {
        let tracked = tracked_sub_mut(&mut pool, sub_id);
        tracked.progress.start_pending_ingestion(
            missing_id,
            PendingIngestion {
                target: FullHistoryRelayFilter::new(
                    relay.clone(),
                    initial_pkg.policy(),
                    filter.clone(),
                ),
                started_at: Instant::now(),
                retries_started: 0,
            },
        );
    }

    {
        apply_send_session_with(&mut pool, wakeup, |_pool, session| {
            update_full_history_targets(session, sub_id, vec![updated_target]);
        })
    }

    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.rounds_started, 7);
    assert!(
        tracked.progress.pending_neg_sets.is_empty(),
        "policy-only target changes should not enqueue a fresh local-set build"
    );

    let snapshot_target = &tracked.snapshot.relay_filters()[0];
    assert_eq!(
        snapshot_target.demand_priority(),
        RelayDemandPriority::Critical
    );
    assert_eq!(
        snapshot_target.relay_policy.routing_preference(),
        crate::relay::RelayRoutingPreference::RequireDedicated
    );

    let pending = tracked
        .progress
        .pending_ingestion(&missing_id)
        .expect("policy-only change should retain in-flight fetch");
    assert_eq!(
        pending.target.demand_priority(),
        RelayDemandPriority::Critical
    );
    assert_eq!(
        pending.target.relay_policy.routing_preference(),
        crate::relay::RelayRoutingPreference::RequireDedicated
    );
}

#[tokio::test]
async fn upsert_full_history_sub_refreshes_stored_fetch_policy() {
    let mut pool = ready_pool();
    let present = HashSet::new();
    let wakeup = MockWakeup::default();
    let relay = relay_url("stored-fetch-policy-refresh");
    let initial_pkg = RelayUrlPkgs::new(
        HashSet::from([relay.clone()]),
        crate::relay::RelayUrlPolicy::remote_advertised(
            RelayDemandPriority::Opportunistic,
            RelayRoutingPreference::NoPreference,
        ),
    );
    let updated_pkg = RelayUrlPkgs::new(
        HashSet::from([relay.clone()]),
        crate::relay::RelayUrlPolicy::explicit(
            RelayDemandPriority::Critical,
            RelayRoutingPreference::RequireDedicated,
        ),
    );

    let history_id = {
        apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
            upsert_full_history_targets(
                pool,
                session,
                vec![FullHistoryTarget::new(trivial_filter(), vec![initial_pkg])],
            )
        })
    };
    clear_pending_neg_sets(&mut pool, history_id);
    seed_relay_need(&mut pool, &relay, history_id, note_id(0x46));

    let fetch_id = OutboxSubId(pool.id_registry.next_sub_id_value_for_test());
    let mut fetch_session = TestOutboxCommands::default();
    pool.poll_full_history(&mut fetch_session);
    apply_local_presence_requests_into(&mut pool, &present, &mut fetch_session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut fetch_session);
    let _ = collect_test_command_batch(&mut pool, fetch_session);

    let stored = pool
        .subs
        .get(&fetch_id)
        .expect("full-history fetch should be stored");
    assert_eq!(stored.relay_url_source, RelayUrlSource::RemoteAdvertised);
    assert_eq!(stored.demand_priority, RelayDemandPriority::Opportunistic);
    assert_eq!(
        stored.routing_preference,
        RelayRoutingPreference::NoPreference
    );

    {
        apply_send_session_with(&mut pool, wakeup, |_pool, session| {
            update_full_history_targets(
                session,
                history_id,
                vec![FullHistoryTarget::new(trivial_filter(), vec![updated_pkg])],
            );
        })
    }

    let stored = pool
        .subs
        .get(&fetch_id)
        .expect("policy refresh should not remove the stored fetch");
    assert_eq!(stored.relay_url_source, RelayUrlSource::Explicit);
    assert_eq!(stored.demand_priority, RelayDemandPriority::Critical);
    assert_eq!(
        stored.routing_preference,
        RelayRoutingPreference::RequireDedicated
    );
}

#[tokio::test]
async fn full_history_snapshot_preserves_explicit_bounds() {
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let relay = relay_url("bounded");

    let raw_filter = Filter::new().kinds(vec![1]).limit(10).since(123).build();
    let sub_id = subscribe_with_history(
        &mut pool,
        wakeup,
        vec![raw_filter.clone()],
        vec![raw_filter.clone()],
        [relay],
    );

    let snapshot = pool
        .full_history_snapshot(sub_id)
        .expect("full-history snapshot should exist");
    let filters = snapshot.filters();

    assert_eq!(filters.len(), 1);
    assert_eq!(filters[0].limit(), raw_filter.limit());
    assert_eq!(filters[0].since(), raw_filter.since());
}
#[tokio::test]
async fn remove_full_history_sub_clears_tracker_and_shared_progress() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("remove");

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay]);
    let tracked = tracked_sub_mut(&mut pool, sub_id);
    tracked.progress.start_pending_ingestion(
        note_id(7),
        pending_ingestion(relay_url("pending"), Instant::now()),
    );
    seed_fetch_retry(tracked, note_id(9), relay_url("failed"), Instant::now());

    remove_full_history(&mut pool, wakeup, sub_id);

    assert!(!is_tracked(&pool, sub_id));
}
#[tokio::test]
async fn poll_negentropy_state_machine_retains_ready_storage_until_relay_available() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut pool = counting_ready_pool(Arc::clone(&calls));
    let relay = relay_url("ready");

    let sub_id = subscribe_history_only(
        &mut pool,
        MockWakeup::default(),
        trivial_filter(),
        [relay.clone()],
    );
    assert_eq!(apply_ready_local_set_requests_counted(&mut pool, &calls), 1);
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
    assert_eq!(
        pending_neg_set_relays(&pool, sub_id),
        HashSet::from([relay.clone()])
    );

    poll_negentropy_state_machine_with_ready_local_sets(&mut pool);
    poll_negentropy_state_machine_with_ready_local_sets(&mut pool);

    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
    assert_eq!(
        pending_neg_set_relays(&pool, sub_id),
        HashSet::from([relay])
    );
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
}
#[tokio::test]
async fn poll_negentropy_state_machine_retains_unsent_filter_until_relay_available() {
    let mut pool = ready_pool();
    let relay = relay_url("large-filter");
    let filter = filter_larger_than_default_json_buffer();
    let history_id = subscribe_history_only(
        &mut pool,
        MockWakeup::default(),
        vec![filter.clone()],
        [relay.clone()],
    );

    poll_negentropy_state_machine_with_ready_local_sets(&mut pool);

    let tracked = tracked_sub(&pool, history_id);
    assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
    let pending = &tracked.progress.pending_neg_sets[0];
    assert_eq!(pending.relays, vec![relay]);
    assert!(pending.filter.same_canonical_attributes(&filter));
}

#[tokio::test]
async fn short_max_subid_length_blocks_full_history_negentropy() {
    let (_relay_task, relay, captured, _notify) = create_text_capture_relay().await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let history_id =
        subscribe_history_only(&mut pool, wakeup.clone(), trivial_filter(), [relay.clone()]);

    let rejected = pool.apply_unsupported_subid_length(&relay, 8);
    assert_eq!(
        rejected,
        Nip11ApplyOutcome::UnsupportedSubIdLength {
            max_subid_length: 8
        }
    );
    assert_eq!(pool.relays.len(), 1);
    assert!(!relay_data(&pool, &relay).supports_relay_subscription_ids());
    assert!(
        pool.relay_transport_demand(&relay).is_none(),
        "short max_subid_length must clear relay transport demand"
    );
    assert_active_sessions(&pool, &relay, 0);

    let mut staged_session = TestOutboxCommands::default();
    for _ in 0..5 {
        assert!(!relay_data(&pool, &relay).supports_relay_subscription_ids());
        poll_full_history_with_ready_local_sets(&mut pool, &mut staged_session);
        assert!(!relay_data(&pool, &relay).supports_relay_subscription_ids());
        assert_active_sessions(&pool, &relay, 0);
        assert_eq!(captured_count(&captured, NEG_OPEN_PREFIX), 0);
        drive_transport_once(&mut pool, &wakeup);
        assert!(
            pool.relay_transport_demand(&relay).is_none(),
            "unsupported relay must not retain subscription-id transport demand"
        );
        receive_active_relays(&mut pool);
        assert_eq!(captured_count(&captured, NEG_OPEN_PREFIX), 0);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let frames = captured.lock().expect("lock captured frames").clone();
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame.starts_with(NEG_OPEN_PREFIX))
            .count(),
        0,
        "short max_subid_length must suppress NEG-OPEN with UUID session ids: {frames:?}"
    );
    assert_eq!(
        captured_count(&captured, NEG_CLOSE_PREFIX),
        0,
        "short max_subid_length must not trigger over-length NEG-CLOSE cleanup"
    );
    assert_active_sessions(&pool, &relay, 0);
    {
        let tracked = tracked_sub(&pool, history_id);
        assert!(
            tracked.progress.pending_neg_sets.is_empty(),
            "unsupported relay must not retain pending full-history negentropy"
        );
        assert!(
            tracked.progress.retry_states.is_empty(),
            "unsupported relay must not retain delayed full-history retry state"
        );
    }
    assert!(
        pool.full_history_catchup_complete(history_id),
        "unsupported relay must not block full-history catchup"
    );

    let compatible = pool.apply_relay_limit_update(&relay, RelayLimitations::default());
    assert_eq!(compatible, Nip11ApplyOutcome::Unchanged);
    assert!(!relay_data(&pool, &relay).supports_relay_subscription_ids());
    poll_full_history_with_ready_local_sets(&mut pool, &mut staged_session);
    drive_transport_once(&mut pool, &wakeup);
    assert_eq!(
        captured_count(&captured, NEG_OPEN_PREFIX),
        0,
        "compatible subid length must not live-restore NEG-OPEN"
    );
    assert_active_sessions(&pool, &relay, 0);
}

#[tokio::test]
async fn poll_negentropy_state_machine_drops_pending_when_full_history_removed() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("live");

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay]);
    remove_full_history(&mut pool, wakeup, sub_id);

    poll_negentropy_state_machine_with_ready_local_sets(&mut pool);

    assert!(!is_tracked(&pool, sub_id));
}
#[tokio::test]
async fn stage_need_fetches_skips_known_ids_and_batches_by_relay() {
    let mut pool = OutboxPool::default();
    let present = HashSet::from([note_id(3)]);
    let wakeup = MockWakeup::default();
    let relay = relay_url("fetch");
    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);

    let tracked = tracked_sub_mut(&mut pool, sub_id);
    tracked
        .progress
        .start_pending_ingestion(note_id(1), pending_ingestion(relay.clone(), Instant::now()));
    seed_fetch_retry(
        tracked,
        note_id(2),
        relay.clone(),
        Instant::now() + Duration::from_secs(60),
    );

    let mut staged_session = TestOutboxCommands::default();
    stage_need_fetches_for_test(
        &mut pool,
        vec![
            full_history_need(sub_id, relay.clone(), note_id(1)),
            full_history_need(sub_id, relay.clone(), note_id(2)),
            full_history_need(sub_id, relay.clone(), note_id(3)),
            full_history_need(sub_id, relay.clone(), note_id(4)),
            full_history_need(sub_id, relay.clone(), note_id(5)),
        ],
        &mut staged_session,
        &present,
    );

    let tracked = tracked_sub(&pool, sub_id);
    assert!(tracked.progress.pending_ingestion(&note_id(1)).is_some());
    assert!(tracked.progress.pending_ingestion(&note_id(4)).is_some());
    assert!(tracked.progress.pending_ingestion(&note_id(5)).is_some());
    assert!(tracked.progress.pending_ingestion(&note_id(2)).is_none());
    assert!(tracked.progress.pending_ingestion(&note_id(3)).is_none());
    assert_eq!(pool.id_registry.next_sub_id_value_for_test(), 2);

    let oneshot = staged_session
        .full_history_fetches
        .get(&OutboxSubId(1))
        .expect("one batched fetch should be staged");
    let _ = oneshot;
}

#[tokio::test]
async fn stage_need_fetches_preserves_remote_advertised_relay_package() {
    let mut pool = ready_pool();
    let present = HashSet::new();
    let wakeup = MockWakeup::default();
    let relay = NormRelayUrl::new("wss://relay-remote-fetch.example.com").unwrap();
    let relay_pkgs = RelayUrlPkgs::new(
        HashSet::from([relay.clone()]),
        crate::relay::RelayUrlPolicy::remote_advertised(
            crate::relay::RelayDemandPriority::Opportunistic,
            crate::relay::RelayRoutingPreference::NoPreference,
        ),
    );
    let history_id = {
        apply_send_session_with(&mut pool, wakeup, |pool, session| {
            upsert_full_history(
                pool,
                session,
                FullHistoryConfig::new(trivial_filter()),
                vec![relay_pkgs.clone()],
            )
        })
    };

    let missing_id = note_id(0x61);
    seed_relay_need(&mut pool, &relay, history_id, missing_id);

    let request_id = pool.id_registry.next_sub_id_value_for_test();
    let mut staged_session = TestOutboxCommands::default();
    pool.poll_full_history_deadline(&mut staged_session);
    apply_local_presence_requests_into(&mut pool, &present, &mut staged_session);
    pool.poll_full_history_deadline(&mut staged_session);

    let task = staged_session
        .full_history_fetches
        .get(&OutboxSubId(request_id))
        .expect("remote-advertised need should stage a fetch");
    let fetch = task;
    assert_eq!(fetch.subscribe.relays.urls, HashSet::from([relay]));
    assert_eq!(
        fetch.subscribe.relays.source(),
        RelayUrlSource::RemoteAdvertised
    );
    assert_eq!(
        fetch.subscribe.relays.demand_priority(),
        RelayDemandPriority::Opportunistic
    );
    assert_eq!(
        fetch.subscribe.relays.routing_preference(),
        RelayRoutingPreference::NoPreference
    );
}

#[tokio::test]
async fn remote_advertised_full_history_drops_blocked_urls_before_admission() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = NormRelayUrl::new("wss://127.0.0.1").unwrap();
    let relay_pkgs = RelayUrlPkgs::new(
        HashSet::from([relay.clone()]),
        crate::relay::RelayUrlPolicy::remote_advertised(
            crate::relay::RelayDemandPriority::Opportunistic,
            crate::relay::RelayRoutingPreference::NoPreference,
        ),
    );

    let history_id = {
        apply_send_session_with(&mut pool, wakeup, |pool, session| {
            let history_id = pool.id_registry.next_full_history_id();
            let kept = session.upsert_full_history(
                history_id,
                FullHistoryConfig::new(trivial_filter()),
                vec![relay_pkgs],
            );
            assert!(
                !kept,
                "empty filtered full-history declaration should report removal"
            );
            history_id
        })
    };

    assert!(
        !pool.relays.contains_key(&relay),
        "blocked remote-advertised full-history relay should not reach admission"
    );
    assert!(
        !is_tracked(&pool, history_id),
        "empty filtered full-history declaration should not retain tracked state"
    );
}

#[tokio::test]
async fn modifying_full_history_to_blocked_remote_advertised_urls_removes_id() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let initial_relay = relay_url("history-modify-kept");
    let history_id = {
        apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
            upsert_full_history(
                pool,
                session,
                FullHistoryConfig::new(trivial_filter()),
                test_history_relay_pkgs(relay_set([initial_relay])),
            )
        })
    };
    assert!(is_tracked(&pool, history_id));

    let blocked_relay = NormRelayUrl::new("wss://127.0.0.1").unwrap();
    let blocked_pkgs = RelayUrlPkgs::new(
        HashSet::from([blocked_relay]),
        crate::relay::RelayUrlPolicy::remote_advertised(
            crate::relay::RelayDemandPriority::Opportunistic,
            crate::relay::RelayRoutingPreference::NoPreference,
        ),
    );
    let kept = {
        apply_send_session_with(&mut pool, wakeup, |_, session| {
            update_full_history(
                session,
                history_id,
                FullHistoryConfig::new(trivial_filter()),
                vec![blocked_pkgs],
            )
        })
    };

    assert!(
        !kept,
        "normalized-empty full-history modification should report removal"
    );
    assert!(
        !is_tracked(&pool, history_id),
        "normalized-empty full-history modification should remove tracked state"
    );
}

#[tokio::test]
async fn full_history_initial_open_merges_package_demand_with_pending_work() {
    let (_relay_task, relay, _captured, _notify) = create_text_capture_relay().await;
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    {
        let relay_pkgs = RelayUrlPkgs::new(
            HashSet::from([relay.clone()]),
            crate::relay::RelayUrlPolicy::explicit(
                crate::relay::RelayDemandPriority::Important,
                crate::relay::RelayRoutingPreference::PreferDedicated,
            ),
        );
        apply_send_session_with(&mut pool, wakeup, |pool, session| {
            upsert_full_history(
                pool,
                session,
                FullHistoryConfig::new(trivial_filter()),
                vec![relay_pkgs],
            );
        })
    }

    assert!(
        pool.relay_transport_demand(&relay).is_some(),
        "important full-history package demand should survive pending-neg-set demand"
    );
}

#[tokio::test]
async fn full_history_capacity_work_requires_ready_local_set() {
    let mut pool = ready_pool();
    let relay = relay_url("capacity-ready-local-set");
    let history_id = subscribe_unbounded(&mut pool, MockWakeup::default(), [relay.clone()]);

    assert!(
        pool.service
            .full_history
            .ids_with_relay_transport_demand(&relay)
            .contains(&history_id),
        "pending local-set build should still contribute relay connection demand"
    );
    assert!(
        pool.service
            .full_history
            .ids_with_ready_pending_neg_set_for_relay(&relay)
            .is_empty(),
        "capacity must not be requested before local-set storage is ready"
    );

    assert_eq!(apply_ready_local_set_requests(&mut pool), 1);

    assert_eq!(
        pool.service
            .full_history
            .ids_with_ready_pending_neg_set_for_relay(&relay),
        vec![history_id],
        "ready local-set storage should become eligible for negentropy capacity"
    );
}

#[tokio::test]
async fn duplicate_full_history_relay_packages_merge_before_needs() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = NormRelayUrl::new("wss://relay-duplicate-history.example.com").unwrap();
    let remote_pkg = RelayUrlPkgs::new(
        HashSet::from([relay.clone()]),
        crate::relay::RelayUrlPolicy::remote_advertised(
            crate::relay::RelayDemandPriority::Opportunistic,
            crate::relay::RelayRoutingPreference::NoPreference,
        ),
    );
    let explicit_pkg = RelayUrlPkgs::new(
        HashSet::from([relay.clone()]),
        crate::relay::RelayUrlPolicy::explicit(
            crate::relay::RelayDemandPriority::Critical,
            crate::relay::RelayRoutingPreference::RequireDedicated,
        ),
    );
    let history_id = {
        apply_send_session_with(&mut pool, wakeup, |pool, session| {
            upsert_full_history(
                pool,
                session,
                FullHistoryConfig::new(trivial_filter()),
                vec![remote_pkg, explicit_pkg],
            )
        })
    };

    let tracked = tracked_sub(&pool, history_id);
    let tracked_relay_pkgs = tracked.snapshot.relay_pkgs();
    assert_eq!(tracked_relay_pkgs.len(), 1);
    let relay_pkgs = &tracked_relay_pkgs[0];
    assert_eq!(relay_pkgs.urls, HashSet::from([relay.clone()]));
    assert_eq!(relay_pkgs.source(), RelayUrlSource::Explicit);
    assert_eq!(relay_pkgs.demand_priority(), RelayDemandPriority::Critical);
    assert_eq!(
        relay_pkgs.routing_preference(),
        RelayRoutingPreference::RequireDedicated
    );

    let request_id = pool.id_registry.next_sub_id_value_for_test();
    queue_snapshot_need(
        &mut pool,
        history_id,
        &relay,
        &trivial_filter()[0],
        note_id(0x62),
    );
    let mut staged_session = TestOutboxCommands::default();
    pool.poll_full_history_deadline(&mut staged_session);
    apply_local_presence_requests_into(&mut pool, &HashSet::new(), &mut staged_session);
    pool.poll_full_history_deadline(&mut staged_session);

    let task = staged_session
        .full_history_fetches
        .get(&OutboxSubId(request_id))
        .expect("merged duplicate relay package should stage a fetch");
    let fetch = task;
    assert_eq!(fetch.subscribe.relays.source(), RelayUrlSource::Explicit);
    assert_eq!(
        fetch.subscribe.relays.demand_priority(),
        RelayDemandPriority::Critical
    );
    assert_eq!(
        fetch.subscribe.relays.routing_preference(),
        RelayRoutingPreference::RequireDedicated
    );
}

#[tokio::test]
async fn stage_need_fetches_retains_alternate_relay_while_fetch_is_pending() {
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let first_relay = relay_url("dedupe-first");
    let second_relay = relay_url("dedupe-second");
    let sub_id = subscribe_unbounded(
        &mut pool,
        wakeup.clone(),
        [first_relay.clone(), second_relay.clone()],
    );
    let missing = note_id(6);

    let mut staged_session = TestOutboxCommands::default();
    stage_need_fetches_for_test(
        &mut pool,
        vec![
            full_history_need(sub_id, first_relay.clone(), missing),
            full_history_need(sub_id, second_relay.clone(), missing),
        ],
        &mut staged_session,
        &HashSet::new(),
    );

    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.progress.pending_ingestion_len(), 1);
    assert_eq!(
        tracked
            .progress
            .pending_ingestion(&missing)
            .expect("active fetch should be tracked")
            .target
            .relay,
        first_relay
    );
    assert!(
        tracked
            .progress
            .fetch_candidate_waiting(&missing, &second_relay),
        "alternate relay should be retained while the first fetch is active"
    );
    assert_eq!(staged_session.full_history_fetches.len(), 1);
}

#[tokio::test]
async fn stage_need_fetches_batches_local_presence_checks() {
    let mut pool = OutboxPool::default();
    let present = HashSet::from([note_id(3)]);
    let wakeup = MockWakeup::default();
    let relay = relay_url("batch");
    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);

    let tracked = tracked_sub_mut(&mut pool, sub_id);
    tracked
        .progress
        .start_pending_ingestion(note_id(1), pending_ingestion(relay.clone(), Instant::now()));
    seed_fetch_retry(
        tracked,
        note_id(2),
        relay.clone(),
        Instant::now() + Duration::from_secs(60),
    );

    let (output, initial) = pool.service.full_history.stage_need_fetches(
        vec![
            full_history_need(sub_id, relay.clone(), note_id(1)),
            full_history_need(sub_id, relay.clone(), note_id(2)),
            full_history_need(sub_id, relay.clone(), note_id(3)),
            full_history_need(sub_id, relay.clone(), note_id(4)),
            full_history_need(sub_id, relay, note_id(5)),
        ],
        Instant::now(),
    );
    assert!(
        output.fetch_requests.is_empty(),
        "presence check should run before fetch staging"
    );
    pool.record_full_history_local_output(output);
    assert!(initial.is_empty());

    let requests = take_local_presence_requests(&mut pool);
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].candidate_ids,
        HashSet::from([note_id(2), note_id(3), note_id(4), note_id(5)])
    );
    let request = requests.into_iter().next().expect("presence request");
    let mut missing_ids = request.candidate_ids.clone();
    missing_ids.retain(|id| !present.contains(id));
    let (applied, output) =
        pool.service
            .apply_full_history_local_presence_ready(FullHistoryLocalPresenceResult {
                request_id: request.request_id,
                missing_ids,
                already_local_ids: present,
            });
    let mut session = TestOutboxCommands::default();
    assert!(applied);
    pool.record_full_history_runtime_output(output, &mut session);
    assert_eq!(session.full_history_fetches.len(), 1);
}

#[tokio::test]
async fn poll_full_history_schedules_fresh_round_when_all_needs_are_already_local() {
    let mut pool = counting_ready_pool(Arc::new(AtomicUsize::new(0)));
    let relay = relay_url("already-local");
    let present = note_id(0x44);
    let present_ids = HashSet::from([present]);
    let history_id = subscribe_unbounded(&mut pool, MockWakeup::default(), [relay.clone()]);
    clear_pending_neg_sets(&mut pool, history_id);
    seed_relay_need(&mut pool, &relay, history_id, present);

    let mut session = TestOutboxCommands::default();
    pool.poll_full_history(&mut session);
    apply_local_presence_requests_into(&mut pool, &present_ids, &mut session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut session);

    assert!(session.is_empty());
    assert!(tracked_sub(&pool, history_id)
        .progress
        .pending_ingestion_is_empty());
    assert_eq!(tracked_sub(&pool, history_id).rounds_started, 2);
    assert_eq!(
        tracked_sub(&pool, history_id)
            .progress
            .pending_neg_sets
            .len(),
        1,
        "already-local needs should complete fetch planning and schedule fresh verification"
    );
}

#[tokio::test]
async fn poll_full_history_schedules_fresh_round_when_fetch_retry_is_now_local() {
    let mut pool = ready_pool();
    let relay = relay_url("failed-then-local");
    let present = note_id(0x45);
    let present_ids = HashSet::from([present]);
    let history_id = subscribe_unbounded(&mut pool, MockWakeup::default(), [relay.clone()]);
    clear_pending_neg_sets(&mut pool, history_id);
    seed_fetch_retry(
        tracked_sub_mut(&mut pool, history_id),
        present,
        relay.clone(),
        Instant::now() + Duration::from_secs(60),
    );
    seed_relay_need(&mut pool, &relay, history_id, present);

    let mut session = TestOutboxCommands::default();
    pool.poll_full_history(&mut session);
    apply_local_presence_requests_into(&mut pool, &present_ids, &mut session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut session);

    let tracked = tracked_sub(&pool, history_id);
    assert!(session.is_empty());
    assert!(tracked.progress.pending_ingestion_is_empty());
    assert!(!tracked.progress.fetch_retry_waiting(&present, &relay));
    assert_eq!(tracked.rounds_started, 2);
    assert_eq!(
        tracked.progress.pending_neg_sets.len(),
        1,
        "already-local needs should not be hidden by stale fetch retry state"
    );
}

#[tokio::test]
async fn poll_full_history_rebuilds_local_set_when_already_local_needs_complete_round() {
    let mut pool = ready_pool();
    let relay_a = relay_url("already-local-a");
    let relay_b = relay_url("already-local-b");
    let present = note_id(0x46);
    let present_ids = HashSet::from([present]);
    let history_id = subscribe_unbounded(
        &mut pool,
        MockWakeup::default(),
        [relay_a.clone(), relay_b.clone()],
    );
    clear_pending_neg_sets(&mut pool, history_id);
    let output = pool.service.full_history.schedule_round(history_id);
    pool.record_full_history_local_output(output);
    {
        let tracked = tracked_sub_mut(&mut pool, history_id);
        assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
        tracked.progress.pending_neg_sets[0].relays = vec![relay_b.clone()];
    }
    seed_relay_need(&mut pool, &relay_a, history_id, present);

    let mut session = TestOutboxCommands::default();
    pool.poll_full_history(&mut session);
    apply_local_presence_requests_into(&mut pool, &present_ids, &mut session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut session);

    let tracked = tracked_sub(&pool, history_id);
    assert!(session.is_empty());
    assert_eq!(tracked.rounds_started, 3);
    assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
    let pending_relays: HashSet<NormRelayUrl> = tracked.progress.pending_neg_sets[0]
        .relays
        .iter()
        .cloned()
        .collect();
    assert_eq!(pending_relays, HashSet::from([relay_a, relay_b]));
}

#[tokio::test]
async fn live_task_subscribe_uses_explicit_full_history() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("staged-history-filter");
    let live_filter = Filter::new().kinds(vec![1]).limit(500).build();
    let history_filter = Filter::new().kinds(vec![1]).since(123).build();

    let (live_id, history_id) = apply_send_session_with(&mut pool, wakeup, |pool, session| {
        let live_id = subscribe(
            pool,
            session,
            vec![live_filter.clone()],
            test_relay_pkgs(relay_set([relay.clone()])),
        );
        let history_id = upsert_full_history(
            pool,
            session,
            FullHistoryConfig::new(vec![history_filter.clone()]),
            test_history_relay_pkgs(relay_set([relay.clone()])),
        );
        (live_id, history_id)
    });

    let tracked = tracked_sub(&pool, history_id);
    assert_eq!(tracked_snapshot_relays(tracked), vec![relay]);
    assert_eq!(
        filters_json(&tracked.snapshot.filters()),
        filters_json(std::slice::from_ref(&history_filter))
    );
    assert_eq!(
        filters_json(pool.filters(&live_id).expect("live filters")),
        filters_json(std::slice::from_ref(&live_filter))
    );
}

#[tokio::test]
async fn live_task_full_modify_from_none_tracks_full_history() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("staged-enable");

    let sub_id = FullHistorySubId(0);
    assert!(!is_tracked(&pool, sub_id));

    {
        apply_send_session_with(&mut pool, wakeup, |_, session| {
            let filters = trivial_filter();
            update_full_history(
                session,
                sub_id,
                FullHistoryConfig::new(filters),
                test_history_relay_pkgs(relay_set([relay])),
            );
        })
    }

    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.snapshot.id, sub_id);
    assert!(!tracked.progress.pending_neg_sets.is_empty());
}
#[tokio::test]
async fn live_task_full_modify_uses_explicit_full_history() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("staged-modify-history-filter");
    let live_id = {
        apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
            subscribe(
                pool,
                session,
                vec![Filter::new().kinds(vec![1]).limit(500).build()],
                test_relay_pkgs(relay_set([relay.clone()])),
            )
        })
    };
    let history_id = subscribe_with_history(
        &mut pool,
        wakeup.clone(),
        vec![Filter::new().kinds(vec![1]).limit(500).build()],
        vec![Filter::new().kinds(vec![1]).build()],
        [relay.clone()],
    );
    let next_live_filter = Filter::new().kinds(vec![1]).limit(250).build();
    let next_history_filter = Filter::new().kinds(vec![1]).since(456).build();

    {
        apply_send_session_with(&mut pool, wakeup, |pool, session| {
            modify_full(
                pool,
                session,
                live_id,
                vec![next_live_filter.clone()],
                relay_set([relay.clone()]),
            );
            update_full_history(
                session,
                history_id,
                FullHistoryConfig::new(vec![next_history_filter.clone()]),
                test_history_relay_pkgs(relay_set([relay.clone()])),
            );
        })
    }

    let tracked = tracked_sub(&pool, history_id);
    assert_eq!(tracked_snapshot_relays(tracked), vec![relay]);
    assert_eq!(
        filters_json(&tracked.snapshot.filters()),
        filters_json(std::slice::from_ref(&next_history_filter))
    );
    assert_eq!(
        filters_json(pool.filters(&live_id).expect("live filters")),
        filters_json(std::slice::from_ref(&next_live_filter))
    );
}
#[tokio::test]
async fn live_task_filter_modify_preserves_explicit_full_history() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("drop-filter-preserves-history");
    let live_filter = Filter::new().kinds(vec![1]).limit(500).build();
    let history_filter = Filter::new().kinds(vec![1]).since(123).build();
    let live_id = {
        apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
            subscribe(
                pool,
                session,
                vec![live_filter],
                test_relay_pkgs(relay_set([relay.clone()])),
            )
        })
    };
    let history_id = subscribe_with_history(
        &mut pool,
        wakeup.clone(),
        vec![Filter::new().kinds(vec![1]).limit(500).build()],
        vec![history_filter.clone()],
        [relay],
    );

    {
        apply_send_session_with(&mut pool, wakeup, |pool, session| {
            modify_filters(
                pool,
                session,
                live_id,
                vec![Filter::new().kinds(vec![1]).limit(250).build()],
            );
        })
    }

    let tracked = tracked_sub(&pool, history_id);
    assert_eq!(
        filters_json(&tracked.snapshot.filters()),
        filters_json(std::slice::from_ref(&history_filter))
    );
}
#[tokio::test]
async fn task_ingest_tracks_full_history_subscriptions() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("drop-track");

    let sub_id = subscribe_unbounded(&mut pool, wakeup, [relay]);

    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.snapshot.id, sub_id);
    assert!(!tracked.progress.pending_neg_sets.is_empty());
}
#[tokio::test]
async fn live_task_full_modify_removes_tracked_full_history() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("drop-modify");

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);
    assert!(is_tracked(&pool, sub_id));

    {
        apply_send_session_with(&mut pool, wakeup, |_pool, session| {
            session.remove_full_history(sub_id);
        })
    }

    assert!(
        !is_tracked(&pool, sub_id),
        "full modify should remove tracked history when no full-history config is provided"
    );
}
#[tokio::test]
async fn full_history_work_guard_is_false_for_quiescent_tracked_subs() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();
    let relay = relay_url("idle");

    let sub_id = subscribe_unbounded(&mut pool, wakeup, [relay]);

    let tracked = tracked_sub_mut(&mut pool, sub_id);
    tracked.progress.clear_round_work();
    apply_ready_local_set_requests(&mut pool);
    tracked_sub_mut(&mut pool, sub_id)
        .progress
        .clear_round_work();

    assert!(!pool.has_full_history_work());
}
#[tokio::test]
async fn poll_full_history_stages_auto_fetches_into_internal_session() {
    let mut pool = OutboxPool::default();
    let present = HashSet::new();
    let wakeup = MockWakeup::default();
    let relay = relay_url("handler");

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);

    pool.relays.entry(relay.clone()).or_insert_with(|| {
        let mut relay_data = CoordinationData::new(RelayLimitations::default());
        let _ =
            relay_data.apply_websocket_opened(&OutboxSubscriptions::default(), Duration::ZERO, 0);
        relay_data
    });
    queue_snapshot_need(&mut pool, sub_id, &relay, &trivial_filter()[0], note_id(7));

    let mut session = TestOutboxCommands::default();
    pool.poll_full_history(&mut session);
    apply_local_presence_requests_into(&mut pool, &present, &mut session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut session);

    assert!(pool.subs.stored_ref(&OutboxSubId(1)).is_none());
    let oneshot = session
        .full_history_fetches
        .get(&OutboxSubId(1))
        .expect("full-history pass should stage a fetch");
    let _ = oneshot;
}
#[tokio::test]
async fn full_history_work_guard_ignores_stale_relay_needs() {
    let mut pool = OutboxPool::default();
    let relay = relay_url("stale-needs");

    pool.relays.entry(relay.clone()).or_insert_with(|| {
        let mut relay_data = CoordinationData::new(RelayLimitations::default());
        let _ =
            relay_data.apply_websocket_opened(&OutboxSubscriptions::default(), Duration::ZERO, 0);
        relay_data
    });
    surface_relay_negentropy_need(&mut pool, &relay, FullHistorySubId(99), note_id(9));

    assert!(
        !pool.has_full_history_work(),
        "unknown full-history owners should be dropped by the input transition"
    );

    let mut staged_session = TestOutboxCommands::default();
    poll_full_history_with_ready_local_sets(&mut pool, &mut staged_session);

    assert!(!pool.has_full_history_work());
}
#[tokio::test]
async fn poll_full_history_timeout_schedules_same_relay_fetch_retry() {
    let mut pool = OutboxPool::default();
    let present = HashSet::new();
    let wakeup = MockWakeup::default();
    let relay = relay_url("timeout");

    let sub_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);
    clear_pending_neg_sets(&mut pool, sub_id);

    let missing_id = note_id(9);
    let tracked = tracked_sub_mut(&mut pool, sub_id);
    tracked.progress.start_pending_ingestion(
        missing_id,
        pending_ingestion(
            relay.clone(),
            Instant::now() - INGESTION_TIMEOUT - Duration::from_millis(1),
        ),
    );

    let mut staged_session = TestOutboxCommands::default();
    pool.poll_full_history_deadline(&mut staged_session);

    let tracked = tracked_sub(&pool, sub_id);
    assert!(tracked.progress.pending_ingestion_is_empty());
    assert!(tracked.progress.fetch_retry_waiting(&missing_id, &relay));
    assert!(pool.next_deadline().is_some());

    let retry_request_id = pool.id_registry.next_sub_id_value_for_test();
    pool.poll_full_history_deadline_at(
        after_ingestion_timeout_and_fetch_retry_backoff(),
        &mut staged_session,
    );
    apply_local_presence_requests_into(&mut pool, &present, &mut staged_session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut staged_session);

    let retry_oneshot = staged_session
        .full_history_fetches
        .get(&OutboxSubId(retry_request_id))
        .expect("same relay fetch retry should stage a fetch");
    let retry_task = retry_oneshot;
    assert_eq!(retry_task.subscribe.relays.urls, HashSet::from([relay]));
    assert_eq!(
        tracked_sub(&pool, sub_id)
            .progress
            .pending_ingestion(&missing_id)
            .expect("retry should be tracked")
            .retries_started,
        1
    );
}

#[tokio::test]
async fn relay_local_fetch_retry_budget_caps_same_relay_fetches() {
    let mut pool = OutboxPool::default();
    let present = HashSet::new();
    let wakeup = MockWakeup::default();
    let relay = relay_url("fetch-budget");
    let history_id = subscribe_unbounded(&mut pool, wakeup, [relay.clone()]);
    clear_pending_neg_sets(&mut pool, history_id);
    let missing_id = note_id(0x91);

    seed_relay_need(&mut pool, &relay, history_id, missing_id);
    let mut session = TestOutboxCommands::default();
    pool.poll_full_history(&mut session);
    apply_local_presence_requests_into(&mut pool, &present, &mut session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut session);
    assert!(tracked_sub(&pool, history_id)
        .progress
        .pending_ingestion(&missing_id)
        .is_some());

    for expected_retries_started in 1..=MAX_FULL_HISTORY_FETCH_RETRIES_PER_RELAY_ID {
        pool.poll_full_history_deadline_at(after_ingestion_timeout(), &mut session);

        let retry_request_id = pool.id_registry.next_sub_id_value_for_test();
        pool.poll_full_history_deadline_at(
            after_ingestion_timeout_and_fetch_retry_backoff(),
            &mut session,
        );
        apply_local_presence_requests_into(&mut pool, &present, &mut session);
        poll_full_history_with_ready_local_sets(&mut pool, &mut session);
        assert!(
            session
                .full_history_fetches
                .contains_key(&OutboxSubId(retry_request_id)),
            "retry {expected_retries_started} should stage a same-relay fetch"
        );
        assert_eq!(
            tracked_sub(&pool, history_id)
                .progress
                .pending_ingestion(&missing_id)
                .expect("retry fetch should be tracked")
                .retries_started,
            expected_retries_started
        );
    }

    pool.poll_full_history_deadline_at(after_ingestion_timeout(), &mut session);

    let tracked = tracked_sub(&pool, history_id);
    assert!(tracked.progress.pending_ingestion_is_empty());
    assert!(!tracked.progress.fetch_retry_waiting(&missing_id, &relay));
    assert!(tracked.progress.fetch_failed(&missing_id, &relay));

    let request_id_after_budget = pool.id_registry.next_sub_id_value_for_test();
    seed_relay_need(&mut pool, &relay, history_id, missing_id);
    pool.poll_full_history_deadline(&mut session);
    apply_local_presence_requests_into(&mut pool, &present, &mut session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut session);
    assert_eq!(
        pool.id_registry.next_sub_id_value_for_test(),
        request_id_after_budget,
        "exhausted relay/id retry state should suppress further same-relay fetches"
    );
}

#[tokio::test]
async fn due_fetch_retry_preserves_retry_count_when_waiting_behind_active_fetch() {
    let mut pool = OutboxPool::default();
    let present = HashSet::new();
    let wakeup = MockWakeup::default();
    let relay_a = relay_url("due-retry-a");
    let relay_b = relay_url("due-retry-b");
    let history_id = subscribe_unbounded(&mut pool, wakeup, [relay_a.clone(), relay_b.clone()]);
    clear_pending_neg_sets(&mut pool, history_id);
    let missing_id = note_id(0x93);

    {
        let progress = &mut tracked_sub_mut(&mut pool, history_id).progress;
        progress.upsert_fetch_retry(
            missing_id,
            relay_filter_target(relay_a.clone()),
            1,
            Instant::now(),
        );
        progress.upsert_fetch_retry(
            missing_id,
            relay_filter_target(relay_b.clone()),
            MAX_FULL_HISTORY_FETCH_RETRIES_PER_RELAY_ID,
            Instant::now(),
        );
    }

    let mut session = TestOutboxCommands::default();
    pool.poll_full_history_deadline(&mut session);
    apply_local_presence_requests_into(&mut pool, &present, &mut session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut session);
    assert_eq!(
        tracked_sub(&pool, history_id)
            .progress
            .pending_ingestion_len(),
        1
    );
    let active_relay = tracked_sub(&pool, history_id)
        .progress
        .pending_ingestion(&missing_id)
        .expect("active fetch should be tracked")
        .target
        .relay
        .clone();
    let deferred_relay = if active_relay == relay_a {
        relay_b.clone()
    } else {
        relay_a.clone()
    };
    assert!(tracked_sub(&pool, history_id)
        .progress
        .fetch_state_suppresses_need(&missing_id, &deferred_relay));

    let request_id = pool.id_registry.next_sub_id_value_for_test();
    pool.poll_full_history_deadline_at(after_ingestion_timeout(), &mut session);
    apply_local_presence_requests_into(&mut pool, &HashSet::new(), &mut session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut session);

    let retry_oneshot = session
        .full_history_fetches
        .get(&OutboxSubId(request_id))
        .expect("deferred due retry should stage a fetch");
    let task = retry_oneshot;
    let retry_relay = task
        .subscribe
        .relays
        .urls
        .iter()
        .next()
        .expect("retry should target one relay")
        .clone();
    assert!(
        retry_relay == relay_a || retry_relay == relay_b,
        "retry relay should be one of the due retry states"
    );
    assert_eq!(
        tracked_sub(&pool, history_id)
            .progress
            .pending_ingestion(&missing_id)
            .expect("deferred retry should be tracked")
            .retries_started,
        if retry_relay == relay_b {
            MAX_FULL_HISTORY_FETCH_RETRIES_PER_RELAY_ID
        } else {
            1
        }
    );
}

#[tokio::test]
async fn timed_out_fetch_on_one_relay_does_not_block_retry_from_another() {
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let relay_a = relay_url("fetch-a");
    let relay_b = relay_url("fetch-b");

    let sub_id = subscribe_unbounded(&mut pool, wakeup, [relay_a.clone(), relay_b.clone()]);

    let missing_id = note_id(9);
    let tracked = tracked_sub_mut(&mut pool, sub_id);
    tracked.progress.start_pending_ingestion(
        missing_id,
        pending_ingestion(
            relay_a.clone(),
            Instant::now() - INGESTION_TIMEOUT - Duration::from_millis(1),
        ),
    );

    let mut staged_session = TestOutboxCommands::default();
    pool.poll_full_history_deadline(&mut staged_session);

    let next_request_id_before = pool.id_registry.next_sub_id_value_for_test();
    stage_need_fetches_for_test(
        &mut pool,
        vec![full_history_need(sub_id, relay_b, missing_id)],
        &mut staged_session,
        &HashSet::new(),
    );

    assert_eq!(
        pool.id_registry.next_sub_id_value_for_test(),
        next_request_id_before + 1
    );
    let oneshot = staged_session
        .full_history_fetches
        .get(&OutboxSubId(next_request_id_before))
        .expect("retry from second relay should stage a fetch");
    let _ = oneshot;
}
#[tokio::test]
async fn timed_out_fetch_retries_other_relay_after_real_dispatch_path() {
    let mut pool = OutboxPool::default();
    let present = HashSet::new();
    let wakeup = MockWakeup::default();
    let relay_a = relay_url("sequence-a");
    let relay_b = relay_url("sequence-b");

    let sub_id = subscribe_unbounded(
        &mut pool,
        wakeup.clone(),
        [relay_a.clone(), relay_b.clone()],
    );

    let missing_id = note_id(11);
    seed_relay_need(&mut pool, &relay_a, sub_id, missing_id);
    seed_relay_need(&mut pool, &relay_b, sub_id, missing_id);

    let first_request_id = pool.id_registry.next_sub_id_value_for_test();
    let mut initial_session = TestOutboxCommands::default();
    pool.poll_full_history(&mut initial_session);
    apply_local_presence_requests_into(&mut pool, &present, &mut initial_session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut initial_session);

    let initial_oneshot = initial_session
        .full_history_fetches
        .get(&OutboxSubId(first_request_id))
        .expect("first surfaced need should stage a fetch");
    let initial_task = initial_oneshot;
    let first_relay = initial_task
        .subscribe
        .relays
        .urls
        .iter()
        .next()
        .expect("initial fetch should target one relay")
        .clone();
    let alternate_relay = if first_relay == relay_a {
        relay_b.clone()
    } else {
        relay_a.clone()
    };
    assert_eq!(
        initial_task.subscribe.relays.urls,
        HashSet::from([first_relay.clone()])
    );
    assert!(
        tracked_sub(&pool, sub_id)
            .progress
            .fetch_candidate_waiting(&missing_id, &alternate_relay),
        "alternate relay should wait behind the active first-relay fetch"
    );

    let retry_request_id = pool.id_registry.next_sub_id_value_for_test();
    let mut timeout_session = TestOutboxCommands::default();
    pool.poll_full_history_deadline_at(after_ingestion_timeout(), &mut timeout_session);
    apply_local_presence_requests_into(&mut pool, &HashSet::new(), &mut timeout_session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut timeout_session);
    let tracked = tracked_sub(&pool, sub_id);
    assert!(tracked
        .progress
        .fetch_retry_waiting(&missing_id, &first_relay));

    assert_eq!(timeout_session.full_history_fetches.len(), 1);
    let retry_oneshot = timeout_session
        .full_history_fetches
        .get(&OutboxSubId(retry_request_id))
        .expect("second relay should stage the fetch after timeout frees the id");
    let retry_task = retry_oneshot;
    assert_eq!(
        retry_task.subscribe.relays.urls,
        HashSet::from([alternate_relay.clone()])
    );
    let tracked = tracked_sub(&pool, sub_id);
    let pending = tracked
        .progress
        .pending_ingestion(&missing_id)
        .expect("second relay fetch should be tracked");
    assert_eq!(pending.target.relay, alternate_relay);
    assert_eq!(pending.retries_started, 0);
    assert!(tracked
        .progress
        .fetch_retry_waiting(&missing_id, &first_relay));
}

#[tokio::test]
async fn full_history_fetch_is_not_deduped_against_active_oneshot() {
    let mut pool = OutboxPool::default();
    let present = HashSet::new();
    let wakeup = MockWakeup::default();
    let relay = relay_url("fetch-dedupe");
    let history_id = subscribe_unbounded(&mut pool, wakeup, [relay.clone()]);
    clear_pending_neg_sets(&mut pool, history_id);

    let missing_id = note_id(0x94);
    let fetch_filter = Filter::new().ids([missing_id.bytes()]).build();
    let mut relay_set = HashSet::new();
    relay_set.insert(relay.clone());

    let mut active_oneshot_session = TestOutboxCommands::default();
    active_oneshot_session.oneshot(
        &mut pool,
        OutboxSubId(900),
        vec![fetch_filter],
        test_relay_pkgs(relay_set),
    );
    let active_oneshot_output = collect_test_command_batch(&mut pool, active_oneshot_session);
    assert!(
        output_touches_relay(&active_oneshot_output, &relay),
        "initial oneshot should be retained"
    );

    seed_relay_need(&mut pool, &relay, history_id, missing_id);
    let fetch_request_id = pool.id_registry.next_sub_id_value_for_test();
    let mut full_history_session = TestOutboxCommands::default();
    pool.poll_full_history(&mut full_history_session);
    apply_local_presence_requests_into(&mut pool, &present, &mut full_history_session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut full_history_session);

    let _ = collect_test_command_batch(&mut pool, full_history_session);
    assert!(
        pool.subs.get(&OutboxSubId(fetch_request_id)).is_some(),
        "full-history fetch should bypass generic active-oneshot dedupe"
    );
    assert!(tracked_sub(&pool, history_id)
        .progress
        .pending_ingestion(&missing_id)
        .is_some());
}

#[tokio::test]
async fn app_oneshot_is_not_deduped_against_active_full_history_fetch() {
    let mut pool = OutboxPool::default();
    let present = HashSet::new();
    let wakeup = MockWakeup::default();
    let relay = relay_url("app-oneshot-after-full-history-fetch");
    let history_id = subscribe_unbounded(&mut pool, wakeup, [relay.clone()]);
    clear_pending_neg_sets(&mut pool, history_id);

    let missing_id = note_id(0x95);
    let fetch_filter = Filter::new().ids([missing_id.bytes()]).build();
    let mut relay_set = HashSet::new();
    relay_set.insert(relay.clone());
    let relays = test_relay_pkgs(relay_set);

    seed_relay_need(&mut pool, &relay, history_id, missing_id);
    let fetch_request_id = pool.id_registry.next_sub_id_value_for_test();
    let mut full_history_session = TestOutboxCommands::default();
    pool.poll_full_history(&mut full_history_session);
    apply_local_presence_requests_into(&mut pool, &present, &mut full_history_session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut full_history_session);

    let _ = collect_test_command_batch(&mut pool, full_history_session);
    assert!(
        pool.subs.get(&OutboxSubId(fetch_request_id)).is_some(),
        "full-history fetch should stage relay work"
    );

    let app_request_id = OutboxSubId(901);
    let mut app_session = TestOutboxCommands::default();
    app_session.oneshot(&mut pool, app_request_id, vec![fetch_filter], relays);
    let app_output = collect_test_command_batch(&mut pool, app_session);

    assert!(
        output_touches_relay(&app_output, &relay) && pool.subs.get(&app_request_id).is_some(),
        "app oneshot should not be suppressed by active full-history fetch"
    );
}

#[tokio::test]
async fn relay_retarget_fetches_alternate_candidate_when_active_fetch_relay_removed() {
    let mut pool = OutboxPool::default();
    let present = HashSet::new();
    let wakeup = MockWakeup::default();
    let relay_a = relay_url("retarget-fetch-a");
    let relay_b = relay_url("retarget-fetch-b");
    let history_id = subscribe_unbounded(
        &mut pool,
        wakeup.clone(),
        [relay_a.clone(), relay_b.clone()],
    );
    let missing_id = note_id(0x92);
    seed_relay_need(&mut pool, &relay_a, history_id, missing_id);
    seed_relay_need(&mut pool, &relay_b, history_id, missing_id);

    let first_request_id = pool.id_registry.next_sub_id_value_for_test();
    let mut initial_session = TestOutboxCommands::default();
    pool.poll_full_history(&mut initial_session);
    apply_local_presence_requests_into(&mut pool, &present, &mut initial_session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut initial_session);
    let initial_oneshot = initial_session
        .full_history_fetches
        .get(&OutboxSubId(first_request_id))
        .expect("first surfaced need should stage a fetch");
    let initial_task = initial_oneshot;
    let first_relay = initial_task
        .subscribe
        .relays
        .urls
        .iter()
        .next()
        .expect("initial fetch should target one relay")
        .clone();
    let alternate_relay = if first_relay == relay_a {
        relay_b.clone()
    } else {
        relay_a.clone()
    };
    assert!(tracked_sub(&pool, history_id)
        .progress
        .fetch_candidate_waiting(&missing_id, &alternate_relay));

    modify_unbounded_history(&mut pool, wakeup, history_id, [alternate_relay.clone()]);
    let mut retained_session = TestOutboxCommands::default();
    apply_local_presence_requests_into(&mut pool, &HashSet::new(), &mut retained_session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut retained_session);

    let tracked = tracked_sub(&pool, history_id);
    let pending = tracked
        .progress
        .pending_ingestion(&missing_id)
        .expect("retained relay should fetch after active relay is removed");
    assert_eq!(pending.target.relay, alternate_relay);
    assert!(!tracked
        .progress
        .fetch_candidate_waiting(&missing_id, &pending.target.relay));
    assert!(!tracked
        .progress
        .fetch_retry_waiting(&missing_id, &first_relay));
}
#[tokio::test]
async fn pending_ingestion_presence_resolves_every_matching_pending_ingestion() {
    let mut pool = OutboxPool::default();
    let fetched = note_id(7);
    let wakeup = MockWakeup::default();
    let relay_a = relay_url("ingest-a");
    let relay_b = relay_url("ingest-b");

    let first_sub = subscribe_unbounded(&mut pool, wakeup.clone(), [relay_a.clone()]);
    let second_sub = subscribe_unbounded(&mut pool, wakeup, [relay_b.clone()]);

    tracked_sub_mut(&mut pool, first_sub)
        .progress
        .start_pending_ingestion(fetched, pending_ingestion(relay_a, Instant::now()));
    tracked_sub_mut(&mut pool, second_sub)
        .progress
        .start_pending_ingestion(fetched, pending_ingestion(relay_b, Instant::now()));

    let mut completed =
        pool.apply_pending_ingestion_presence_result(FullHistoryPendingIngestionPresenceResult {
            stored_ids: HashSet::from([fetched]),
        });
    completed.sort_by_key(|id| id.0);

    assert_eq!(completed, vec![first_sub, second_sub]);
    assert!(tracked_sub(&pool, first_sub)
        .progress
        .pending_ingestion_is_empty());
    assert!(tracked_sub(&pool, second_sub)
        .progress
        .pending_ingestion_is_empty());
}

#[tokio::test]
async fn stage_need_fetches_emits_pending_ingestion_presence_request() {
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let relay = relay_url("pending-ingestion-presence");
    let history_id = subscribe_unbounded(&mut pool, wakeup, [relay.clone()]);
    clear_pending_neg_sets(&mut pool, history_id);

    let missing_id = note_id(0x97);
    let mut session = TestOutboxCommands::default();
    stage_need_fetches_for_test(
        &mut pool,
        vec![full_history_need(history_id, relay, missing_id)],
        &mut session,
        &HashSet::new(),
    );

    let requests = take_pending_ingestion_presence_requests(&mut pool);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].candidate_ids, HashSet::from([missing_id]));
    assert!(requests[0].deadline > Instant::now());
    assert!(tracked_sub(&pool, history_id)
        .progress
        .pending_ingestion(&missing_id)
        .is_some());
}
#[tokio::test]
async fn poll_full_history_timeout_isolated_per_sub() {
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();

    let sub_a = subscribe_unbounded(&mut pool, wakeup.clone(), [relay_url("timeout-a")]);
    let sub_b = subscribe_unbounded(&mut pool, wakeup.clone(), [relay_url("timeout-b")]);

    let tracked_a = tracked_sub_mut(&mut pool, sub_a);
    tracked_a.progress.start_pending_ingestion(
        note_id(1),
        pending_ingestion(
            relay_url("timeout-a"),
            Instant::now() - INGESTION_TIMEOUT - Duration::from_millis(1),
        ),
    );

    let tracked_b = tracked_sub_mut(&mut pool, sub_b);
    tracked_b.progress.start_pending_ingestion(
        note_id(2),
        pending_ingestion(relay_url("timeout-b"), Instant::now()),
    );

    let mut staged_session = TestOutboxCommands::default();
    pool.poll_full_history_deadline(&mut staged_session);

    let tracked_a = tracked_sub(&pool, sub_a);
    assert!(tracked_a
        .progress
        .fetch_retry_waiting(&note_id(1), &relay_url("timeout-a")));
    assert!(tracked_a.progress.pending_ingestion_is_empty());

    let tracked_b = tracked_sub(&pool, sub_b);
    assert!(!tracked_b
        .progress
        .fetch_retry_waiting(&note_id(2), &relay_url("timeout-b")));
    assert!(tracked_b.progress.pending_ingestion(&note_id(2)).is_some());
}
#[tokio::test]
async fn remove_full_history_sub_cancels_active_fetch_oneshots() {
    let mut pool = OutboxPool::default();
    let present = HashSet::new();
    let wakeup = MockWakeup::default();
    let relay = relay_url("fetch-owner");
    let history_id = subscribe_unbounded(&mut pool, wakeup.clone(), [relay.clone()]);
    clear_pending_neg_sets(&mut pool, history_id);
    seed_relay_need(&mut pool, &relay, history_id, note_id(42));

    let fetch_id = OutboxSubId(pool.id_registry.next_sub_id_value_for_test());
    let mut fetch_session = TestOutboxCommands::default();
    pool.poll_full_history(&mut fetch_session);
    apply_local_presence_requests_into(&mut pool, &present, &mut fetch_session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut fetch_session);
    let _ = collect_test_command_batch(&mut pool, fetch_session);
    assert!(
        pool.subs.get(&fetch_id).is_some(),
        "full-history need should stage a fetch oneshot"
    );

    assert!(
        pool.subs.get(&fetch_id).is_some(),
        "fetch oneshot should be active before owner removal"
    );

    remove_full_history(&mut pool, wakeup, history_id);
    assert!(
        pool.subs.get(&fetch_id).is_none(),
        "removing the full-history owner should remove active fetch oneshots"
    );
}

#[tokio::test]
async fn relay_retarget_cancels_only_removed_active_fetch_oneshots() {
    let mut pool = OutboxPool::default();
    let present = HashSet::new();
    let wakeup = MockWakeup::default();
    let retained_relay = relay_url("fetch-retarget-retained");
    let removed_relay = relay_url("fetch-retarget-removed");
    let history_id = subscribe_unbounded(
        &mut pool,
        wakeup.clone(),
        [retained_relay.clone(), removed_relay.clone()],
    );
    clear_pending_neg_sets(&mut pool, history_id);
    seed_relay_need(&mut pool, &retained_relay, history_id, note_id(43));
    seed_relay_need(&mut pool, &removed_relay, history_id, note_id(44));

    let mut fetch_session = TestOutboxCommands::default();
    pool.poll_full_history(&mut fetch_session);
    apply_local_presence_requests_into(&mut pool, &present, &mut fetch_session);
    poll_full_history_with_ready_local_sets(&mut pool, &mut fetch_session);
    let fetch_ids = full_history_fetch_ids_by_relay(&fetch_session);
    let retained_fetch_id = *fetch_ids
        .get(&retained_relay)
        .expect("retained relay should have an active fetch");
    let removed_fetch_id = *fetch_ids
        .get(&removed_relay)
        .expect("removed relay should have an active fetch");
    let _ = collect_test_command_batch(&mut pool, fetch_session);

    modify_unbounded_history(&mut pool, wakeup, history_id, [retained_relay.clone()]);

    assert!(
        pool.subs.get(&removed_fetch_id).is_none(),
        "retargeting away a relay should cancel its active full-history fetch"
    );
    assert!(
        pool.subs.get(&retained_fetch_id).is_some(),
        "retargeting should preserve active fetches for retained relay/filter pairs"
    );
}

#[tokio::test]
async fn transient_retry_does_not_immediately_rebuild_local_set() {
    let (calls, mut pool, relay, sub_id, mut staged_session) = counting_retry_fixture("retry");
    seed_relay_retry(&mut pool, &relay, sub_id);

    poll_full_history_with_ready_local_sets(&mut pool, &mut staged_session);

    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
}

#[tokio::test]
async fn next_deadline_reports_retry_backoff_without_forcing_due() {
    let mut pool = ready_pool();
    let wakeup = MockWakeup::default();

    let relay = relay_url("retry-deadline");
    let sub_id = subscribe_unbounded(&mut pool, wakeup, [relay.clone()]);
    seed_relay_retry(&mut pool, &relay, sub_id);

    let before_poll = Instant::now();
    let mut staged_session = TestOutboxCommands::default();
    poll_full_history_with_ready_local_sets(&mut pool, &mut staged_session);

    let deadline = pool
        .next_deadline()
        .expect("scheduled retry should expose a deadline");
    assert!(deadline > Instant::now());
    assert!(deadline <= before_poll + FULL_HISTORY_RETRY_BACKOFF_BASE + Duration::from_millis(100));
}

#[tokio::test]
async fn relay_neg_err_closed_schedules_full_history_retry() {
    let mut pool = ready_pool();
    let relay = relay_url("retry-neg-err");
    let history_id = subscribe_unbounded(&mut pool, MockWakeup::default(), [relay.clone()]);

    assert_eq!(apply_ready_local_set_requests(&mut pool), 1);
    let _ = pool.service.apply_relay_transport_opened(relay.clone(), 0);
    assert_active_sessions(&pool, &relay, 1);

    let session_id = active_negentropy_session_id(&pool, &relay);
    let before_neg_err = Instant::now();
    let _ = pool
        .service
        .apply_relay_neg_err(&relay, 0, &session_id, "closed: session timeout");

    assert_active_sessions(&pool, &relay, 0);
    assert_full_history_retry_scheduled(&pool, history_id, &relay, before_neg_err);
}

#[tokio::test]
async fn relay_limit_revocation_schedules_full_history_retry() {
    let mut pool = ready_pool();
    let relay = relay_url("retry-revocation");
    let history_id = subscribe_unbounded(&mut pool, MockWakeup::default(), [relay.clone()]);

    assert_eq!(apply_ready_local_set_requests(&mut pool), 1);
    let _ = pool.service.apply_relay_transport_opened(relay.clone(), 0);
    assert_active_sessions(&pool, &relay, 1);

    let before_revocation = Instant::now();
    let _ = pool.service.apply_relay_limit_update(
        &relay,
        RelayLimitations {
            maximum_subs: 0,
            ..Default::default()
        },
    );

    assert_active_sessions(&pool, &relay, 0);
    assert_full_history_retry_scheduled(&pool, history_id, &relay, before_revocation);
}

#[tokio::test]
async fn explicit_local_set_completion_retains_storage_until_relay_capacity() {
    let mut pool = OutboxPool::default();

    let relay = relay_url("local-set-wake");
    let sub_id = subscribe_unbounded(&mut pool, MockWakeup::default(), [relay]);
    let requests = take_local_set_requests(&mut pool);
    assert_eq!(requests.len(), 1);
    assert!(
        pool.next_deadline().is_none(),
        "pending local-set jobs are driven by explicit backend results, not synthetic deadlines"
    );
    let request = requests.into_iter().next().expect("local-set request");
    assert_eq!(request.history_id, sub_id);
    {
        let tracked = tracked_sub(&pool, sub_id);
        assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
        assert_eq!(
            tracked.progress.pending_neg_sets[0].request_id,
            request.request_id
        );
    }

    let mut storage = NegentropyStorageVector::new();
    storage.seal().expect("test negentropy storage should seal");
    assert!(pool.apply_full_history_local_set_ready(sub_id, request.request_id, storage));

    let tracked = tracked_sub(&pool, sub_id);
    assert_eq!(tracked.progress.pending_neg_sets.len(), 1);
    let pending = &tracked.progress.pending_neg_sets[0];
    assert!(
        pending.storage.is_some(),
        "completed local-set storage should be retained until relay capacity is available"
    );
}

#[tokio::test]
async fn dropped_local_set_build_keeps_catchup_incomplete() {
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let relay = relay_url("dropped-local-set");
    let sub_id = subscribe_unbounded(&mut pool, wakeup, [relay]);

    assert!(!pool.full_history_catchup_complete(sub_id));

    let requests = take_local_set_requests(&mut pool);
    assert_eq!(requests.len(), 1);
    let request = requests.into_iter().next().expect("local-set request");
    assert!(pool.apply_full_history_local_set_failed(request.history_id, request.request_id,));

    let tracked = tracked_sub(&pool, sub_id);
    assert!(tracked.progress.pending_neg_sets.is_empty());
    assert!(
        tracked
            .progress
            .retry_states
            .iter()
            .any(|retry| retry.next_retry_at.is_some()),
        "dropped local-set build should retain retry work"
    );
    assert!(!pool.full_history_catchup_complete(sub_id));
}

#[tokio::test]
async fn next_deadline_reports_retry_without_test_provider() {
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();

    let relay = relay_url("retry-no-provider");
    let sub_id = subscribe_unbounded(&mut pool, wakeup, [relay.clone()]);
    seed_relay_retry(&mut pool, &relay, sub_id);

    let mut staged_session = TestOutboxCommands::default();
    poll_full_history_with_ready_local_sets(&mut pool, &mut staged_session);

    assert!(pool.next_deadline().is_some());
}

#[tokio::test]
async fn next_deadline_reports_pending_ingestion_timeout() {
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let relay = relay_url("ingestion-deadline");

    let sub_id = subscribe_unbounded(&mut pool, wakeup, [relay.clone()]);
    tracked_sub_mut(&mut pool, sub_id)
        .progress
        .clear_round_work();
    let started_at = Instant::now();
    tracked_sub_mut(&mut pool, sub_id)
        .progress
        .start_pending_ingestion(note_id(0x77), pending_ingestion(relay, started_at));

    assert_eq!(pool.next_deadline(), Some(started_at + INGESTION_TIMEOUT));
}

#[tokio::test]
async fn next_deadline_reports_ingestion_timeout_without_test_checker() {
    let mut pool = OutboxPool::default();
    let wakeup = MockWakeup::default();
    let relay = relay_url("ingestion-no-checker");

    let sub_id = subscribe_unbounded(&mut pool, wakeup, [relay.clone()]);
    tracked_sub_mut(&mut pool, sub_id)
        .progress
        .clear_round_work();
    let started_at = Instant::now();
    tracked_sub_mut(&mut pool, sub_id)
        .progress
        .start_pending_ingestion(note_id(0x78), pending_ingestion(relay, started_at));

    assert_eq!(pool.next_deadline(), Some(started_at + INGESTION_TIMEOUT));
}

#[tokio::test]
async fn transient_retry_promotes_after_backoff() {
    let (calls, mut pool, relay, sub_id, mut staged_session) = counting_retry_fixture("retry");
    seed_relay_retry(&mut pool, &relay, sub_id);

    poll_full_history_with_counted_ready_local_sets(&mut pool, &mut staged_session, &calls);
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

    force_full_history_retries_due(&mut pool, sub_id);
    pool.poll_full_history_deadline(&mut staged_session);

    poll_full_history_with_counted_ready_local_sets(&mut pool, &mut staged_session, &calls);

    assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);
}

#[tokio::test]
async fn transient_retry_budget_caps_local_set_rebuilds() {
    let (calls, mut pool, relay, sub_id, mut staged_session) = counting_retry_fixture("retry");

    for expected_calls in 2..=(MAX_FULL_HISTORY_RETRIES_PER_RELAY_FILTER + 1) {
        seed_relay_retry(&mut pool, &relay, sub_id);
        poll_full_history_with_counted_ready_local_sets(&mut pool, &mut staged_session, &calls);
        force_full_history_retries_due(&mut pool, sub_id);
        pool.poll_full_history_deadline(&mut staged_session);
        poll_full_history_with_counted_ready_local_sets(&mut pool, &mut staged_session, &calls);
        clear_pending_neg_sets(&mut pool, sub_id);

        assert_eq!(calls.load(AtomicOrdering::SeqCst), expected_calls);
    }

    seed_relay_retry(&mut pool, &relay, sub_id);
    poll_full_history_with_counted_ready_local_sets(&mut pool, &mut staged_session, &calls);
    poll_full_history_with_counted_ready_local_sets(&mut pool, &mut staged_session, &calls);

    assert_eq!(
        calls.load(AtomicOrdering::SeqCst),
        MAX_FULL_HISTORY_RETRIES_PER_RELAY_FILTER + 1
    );
}
