use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use enostr::{
    same_canonical_filter_set, EventClientMessage, EventIngestCapability, EventIngestRequest,
    FullHistoryCapability, FullHistoryLocalPresenceRequest, FullHistoryLocalPresenceResult,
    FullHistoryLocalSetRequest, FullHistoryLocalSetResult,
    FullHistoryPendingIngestionPresenceRequest, FullHistoryPendingIngestionPresenceResult,
    Nip11Capability, Nip11FetchRequest, Nip11LimitationsRaw, NormRelayUrl, NoteId, OutboxEvent,
    OutboxIdRegistry, OutboxService, OutboxServiceConfig, OutboxServiceOutput, Pubkey,
    RelayDemandPriority, RelayId, RelayImplType, RelayReqStatus, RelayRoutingPreference,
    RelayUrlPkgs, RelayUrlPolicy,
};
use hashbrown::HashSet;
use nostrdb::{Filter, Ndb, SendFilter, Transaction};
use tokio::sync::mpsc;

use crate::{
    network::HyperHttpClient,
    relay_limits::fetch_nip11_raw_limits,
    scoped_subs::{
        AuthorOutboxPlanJobCompletion, AuthorOutboxPlanJobRequest, ScopedSubDelta, ScopedSubEffect,
        ScopedSubOutboxOp, ScopedSubOutboxOps, ScopedSubOutput,
    },
    ScopedSubCommand, ScopedSubFact, ScopedSubRuntime,
};

use super::negentropy::build_negentropy_storage;
/// UI/control input sent into the remote bridge thread.
pub(crate) enum RemoteBridgeInput {
    Ui(RemoteIntentBatch),
    SetMaxWebsocketConnections(Option<usize>),
    Shutdown,
}

/// Construction-time bridge configuration.
#[derive(Clone, Copy, Default)]
pub(crate) struct RemoteBridgeConfig {
    pong_timeout: Option<Duration>,
}

impl RemoteBridgeConfig {
    pub(crate) fn with_pong_timeout(mut self, timeout: Duration) -> Self {
        self.pong_timeout = Some(timeout);
        self
    }
}

type BridgeCapabilityFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

type BridgeOutboxService =
    OutboxService<BridgeNip11Capability, BridgeFullHistoryCapability, BridgeEventIngestCapability>;

const PENDING_INGESTION_PRESENCE_BACKOFF_BASE: Duration = Duration::from_millis(25);
const PENDING_INGESTION_PRESENCE_BACKOFF_MAX: Duration = Duration::from_millis(500);
const PENDING_INGESTION_PRESENCE_FINAL_CHECK_LEAD: Duration = Duration::from_millis(5);

#[derive(Clone, Default)]
struct BridgeNip11Capability {
    http_client: HyperHttpClient,
}

impl Nip11Capability for BridgeNip11Capability {
    type Output = Result<Nip11LimitationsRaw, String>;
    type Future = BridgeCapabilityFuture<Self::Output>;

    fn fetch_nip11(&self, request: Nip11FetchRequest) -> Self::Future {
        let http_client = self.http_client.clone();
        Box::pin(async move {
            fetch_nip11_raw_limits(&http_client, &request.relay)
                .await
                .map_err(|error| error.to_string())
        })
    }
}

#[derive(Clone)]
struct BridgeFullHistoryCapability {
    ndb: Ndb,
    job_spawner: crate::jobs::JobSpawner,
}

impl FullHistoryCapability for BridgeFullHistoryCapability {
    type LocalSetOutput = FullHistoryLocalSetResult;
    type LocalSetFuture = BridgeCapabilityFuture<Self::LocalSetOutput>;
    type LocalPresenceOutput = FullHistoryLocalPresenceResult;
    type LocalPresenceFuture = BridgeCapabilityFuture<Self::LocalPresenceOutput>;
    type PendingIngestionPresenceOutput = FullHistoryPendingIngestionPresenceResult;
    type PendingIngestionPresenceFuture =
        BridgeCapabilityFuture<Self::PendingIngestionPresenceOutput>;

    fn build_local_set(&self, request: FullHistoryLocalSetRequest) -> Self::LocalSetFuture {
        let history_id = request.history_id;
        let request_id = request.request_id;
        let Some(filter) = SendFilter::try_clone_from_filter(&request.filter) else {
            return Box::pin(async move {
                FullHistoryLocalSetResult {
                    history_id,
                    request_id,
                    result: None,
                }
            });
        };

        let ndb = self.ndb.clone();
        let job_spawner = self.job_spawner.clone();
        Box::pin(async move {
            let (tx, rx) = tokio::sync::oneshot::channel();
            job_spawner.schedule_then(
                move || Some(build_negentropy_storage(&ndb, filter.as_filter())),
                move |result| {
                    let _ = tx.send(result);
                },
            );
            FullHistoryLocalSetResult {
                history_id,
                request_id,
                result: rx.await.unwrap_or(None),
            }
        })
    }

    fn check_local_presence(
        &self,
        request: FullHistoryLocalPresenceRequest,
    ) -> Self::LocalPresenceFuture {
        let request_id = request.request_id;
        let candidate_ids = request.candidate_ids;
        let fallback_missing_ids = candidate_ids.clone();
        let ndb = self.ndb.clone();
        let job_spawner = self.job_spawner.clone();
        Box::pin(async move {
            let (tx, rx) = tokio::sync::oneshot::channel();
            job_spawner.schedule_then(
                move || full_history_local_presence_result(&ndb, request_id, candidate_ids),
                move |result| {
                    let _ = tx.send(result);
                },
            );
            rx.await.unwrap_or_else(|_| FullHistoryLocalPresenceResult {
                request_id,
                missing_ids: fallback_missing_ids,
                already_local_ids: HashSet::new(),
            })
        })
    }

    fn check_pending_ingestion_presence(
        &self,
        request: FullHistoryPendingIngestionPresenceRequest,
    ) -> Self::PendingIngestionPresenceFuture {
        let ndb = self.ndb.clone();
        Box::pin(pending_ingestion_presence_result(ndb, request))
    }
}

#[derive(Clone)]
struct BridgeEventIngestCapability {
    ndb: Ndb,
}

impl EventIngestCapability for BridgeEventIngestCapability {
    type Future = BridgeCapabilityFuture<()>;

    fn ingest_event(&self, request: EventIngestRequest) -> Self::Future {
        let ndb = self.ndb.clone();
        Box::pin(async move {
            process_relay_event_ingest(&ndb, request);
        })
    }
}

/// Frame-local UI remote work before bridge-side planning.
///
/// Account snapshots are batch section context, not work. A section's
/// `account_changed` is applied before that section's ordered intents.
pub(crate) struct RemoteIntentBatchBuilder {
    sections: Vec<RemoteIntentBatchSection>,
}

impl RemoteIntentBatchBuilder {
    pub(crate) fn new() -> Self {
        Self {
            sections: vec![RemoteIntentBatchSection::default()],
        }
    }

    pub(crate) fn push(&mut self, intent: RemoteIntent) {
        self.current_section_mut().intents.push(intent);
    }

    pub(crate) fn set_account_changed(&mut self, account: BridgeAccountState) {
        if self.current_section().intents.is_empty() {
            self.current_section_mut().account_changed = Some(account);
            return;
        }

        self.sections
            .push(RemoteIntentBatchSection::with_account_changed(account));
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.sections.iter().all(RemoteIntentBatchSection::is_empty)
    }

    pub(crate) fn take(&mut self) -> Option<RemoteIntentBatch> {
        if self.is_empty() {
            return None;
        }

        let sections = std::mem::replace(
            &mut self.sections,
            vec![RemoteIntentBatchSection::default()],
        )
        .into_iter()
        .filter(|section| !section.is_empty())
        .collect();

        Some(RemoteIntentBatch { sections })
    }

    fn current_section(&self) -> &RemoteIntentBatchSection {
        self.sections
            .last()
            .expect("RemoteIntentBatchBuilder always has a current section")
    }

    fn current_section_mut(&mut self) -> &mut RemoteIntentBatchSection {
        self.sections
            .last_mut()
            .expect("RemoteIntentBatchBuilder always has a current section")
    }
}

impl Default for RemoteIntentBatchBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// One drained UI batch.
///
/// Sections are applied in order. If present, `account_changed` is applied
/// before that section's ordered `intents`.
pub(crate) struct RemoteIntentBatch {
    sections: Vec<RemoteIntentBatchSection>,
}

#[derive(Default)]
pub(crate) struct RemoteIntentBatchSection {
    account_changed: Option<BridgeAccountState>,
    intents: Vec<RemoteIntent>,
}

impl RemoteIntentBatch {
    #[cfg(test)]
    pub(crate) fn sections(&self) -> &[RemoteIntentBatchSection] {
        &self.sections
    }
}

impl RemoteIntentBatchSection {
    fn with_account_changed(account: BridgeAccountState) -> Self {
        Self {
            account_changed: Some(account),
            intents: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.account_changed.is_none() && self.intents.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn account_changed(&self) -> Option<&BridgeAccountState> {
        self.account_changed.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn intents(&self) -> &[RemoteIntent] {
        &self.intents
    }
}

/// UI-originating remote intent.
pub(crate) enum RemoteIntent {
    ScopedSub(ScopedSubCommand),
    Fetch(RemoteFetchCommand),
    Publish(RemotePublishCommand),
}

/// Selected-account state cached by the bridge for account-bound remote flows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BridgeAccountState {
    selected_pubkey: Pubkey,
    read_relays: HashSet<NormRelayUrl>,
    write_relays: Vec<RelayId>,
}

impl BridgeAccountState {
    pub(crate) fn new(
        selected_pubkey: Pubkey,
        read_relays: HashSet<NormRelayUrl>,
        write_relays: Vec<RelayId>,
    ) -> Self {
        Self {
            selected_pubkey,
            read_relays,
            write_relays,
        }
    }

    fn selected_pubkey(&self) -> Pubkey {
        self.selected_pubkey
    }

    fn read_relays(&self) -> &HashSet<NormRelayUrl> {
        &self.read_relays
    }

    fn write_relays(&self) -> &[RelayId] {
        &self.write_relays
    }
}

/// Typed transient read command.
pub(crate) enum RemoteFetchCommand {
    SelectedAccountRead { filters: Vec<SendFilter> },
}

/// Typed publish command.
pub(crate) enum RemotePublishCommand {
    SelectedAccountWrite {
        msg: EventClientMessage,
    },
    Explicit {
        msg: EventClientMessage,
        relays: Vec<RelayId>,
    },
}

/// Bridge inputs processed serially by the outbox actor.
enum BridgeActorInput {
    Ui(RemoteIntentBatch),
    SetMaxWebsocketConnections(Option<usize>),
    AuthorOutboxPlanCompleted(AuthorOutboxPlanJobCompletion),
    AuthorOutboxDiscoveryRetryDue,
    Shutdown,
}

#[derive(Clone)]
struct AuthorOutboxEffectRunner {
    ndb: Ndb,
    job_spawner: crate::jobs::JobSpawner,
    inputs: mpsc::UnboundedSender<BridgeActorInput>,
}

impl AuthorOutboxEffectRunner {
    fn apply_effect(&self, effect: ScopedSubEffect) {
        match effect {
            ScopedSubEffect::StartAuthorOutboxPlanJob(request) => {
                self.start_plan_job(request);
            }
        }
    }

    fn start_plan_job(&self, request: AuthorOutboxPlanJobRequest) {
        let ndb = self.ndb.clone();
        let inputs = self.inputs.clone();
        let slot_id = request.slot_id();
        self.job_spawner.schedule_then(
            move || request.run(ndb),
            move |completion| {
                if inputs
                    .send(BridgeActorInput::AuthorOutboxPlanCompleted(completion))
                    .is_err()
                {
                    tracing::debug!(
                        slot_id,
                        "dropping author-outbox plan completion after bridge shutdown"
                    );
                }
            },
        );
    }
}

struct ResolvedFetch {
    relays: HashSet<NormRelayUrl>,
    filters: Vec<Filter>,
}

#[derive(Clone)]
struct ActiveFetch {
    id: enostr::OutboxSubId,
    relays: HashSet<NormRelayUrl>,
    filters: Vec<Filter>,
    pending_relays: HashSet<NormRelayUrl>,
    saw_closed: bool,
}

impl ActiveFetch {
    fn new(id: enostr::OutboxSubId, relays: HashSet<NormRelayUrl>, filters: Vec<Filter>) -> Self {
        Self {
            id,
            pending_relays: relays.clone(),
            relays,
            filters,
            saw_closed: false,
        }
    }

    fn apply_relay_req_status(
        &mut self,
        relay: &NormRelayUrl,
        status: Option<RelayReqStatus>,
    ) -> bool {
        if !self.relays.contains(relay) {
            return false;
        }

        match status {
            Some(RelayReqStatus::InitialQuery) => {
                self.pending_relays.insert(relay.clone());
            }
            Some(RelayReqStatus::Eose) => {
                self.pending_relays.remove(relay);
            }
            Some(RelayReqStatus::Closed) => {
                self.saw_closed = true;
                self.pending_relays.remove(relay);
            }
            None => {
                self.pending_relays.remove(relay);
            }
        }

        self.pending_relays.is_empty()
    }
}

struct StartedFetch {
    active: ActiveFetch,
    output: OutboxServiceOutput,
}

struct ResolvedPublish {
    msg: EventClientMessage,
    relays: Vec<RelayId>,
}

struct FetchPlanner {
    accepted: Vec<ResolvedFetch>,
}

impl FetchPlanner {
    fn new() -> Self {
        Self {
            accepted: Vec::new(),
        }
    }

    fn apply_fetch(&mut self, account: &BridgeAccountState, command: RemoteFetchCommand) {
        match command {
            RemoteFetchCommand::SelectedAccountRead { filters } => {
                let filters = filters.into_iter().map(SendFilter::into_filter).collect();
                self.add_selected_account_read(account, filters);
            }
        }
    }

    fn add_selected_account_read(&mut self, account: &BridgeAccountState, filters: Vec<Filter>) {
        if filters.iter().all(|filter| filter.num_elements() == 0) {
            return;
        }

        let relays = account.read_relays().clone();
        if relays.is_empty() {
            return;
        }
        if self.accepted.iter().any(|accepted| {
            accepted.relays == relays && same_canonical_filter_set(&accepted.filters, &filters)
        }) {
            return;
        }

        self.accepted.push(ResolvedFetch { relays, filters });
    }

    fn into_fetches(self, active_fetches: &[ActiveFetch]) -> Vec<ResolvedFetch> {
        self.accepted
            .into_iter()
            .filter(|fetch| !active_fetch_matches(active_fetches, fetch))
            .collect()
    }
}

fn active_fetch_matches(active_fetches: &[ActiveFetch], fetch: &ResolvedFetch) -> bool {
    active_fetches.iter().any(|active| {
        active.relays == fetch.relays && same_canonical_filter_set(&active.filters, &fetch.filters)
    })
}

struct BridgeOutboxDriver {
    service: BridgeOutboxService,
}

impl BridgeOutboxDriver {
    fn new(ndb: &Ndb, job_spawner: &crate::jobs::JobSpawner, config: RemoteBridgeConfig) -> Self {
        let mut service_config = OutboxServiceConfig::default();
        if let Some(timeout) = config.pong_timeout {
            service_config = service_config.with_pong_timeout(timeout);
        }

        let service = OutboxService::with_capabilities_and_config(
            BridgeNip11Capability::default(),
            BridgeFullHistoryCapability {
                ndb: ndb.clone(),
                job_spawner: job_spawner.clone(),
            },
            BridgeEventIngestCapability { ndb: ndb.clone() },
            service_config,
        );

        Self { service }
    }

    fn id_registry(&self) -> OutboxIdRegistry {
        self.service.id_registry()
    }

    fn next(&mut self) -> impl Future<Output = OutboxServiceOutput> + '_ {
        self.service.next()
    }

    fn apply_scoped_outbox_ops(
        &mut self,
        outbox_ops: ScopedSubOutboxOps,
    ) -> Vec<OutboxServiceOutput> {
        if outbox_ops.is_empty() {
            return Vec::new();
        }

        let mut outputs = Vec::new();
        self.service.begin_effect_turn();
        for op in outbox_ops.into_ops() {
            outputs.push(self.apply_scoped_outbox_op(op));
        }
        outputs.push(self.service.end_effect_turn());
        outputs
    }

    fn apply_scoped_outbox_op(&mut self, op: ScopedSubOutboxOp) -> OutboxServiceOutput {
        match op {
            ScopedSubOutboxOp::SetLive {
                id,
                filters,
                relay_pkgs,
            } => self.service.set_live(id, filters, relay_pkgs),
            ScopedSubOutboxOp::StartFetch {
                id,
                filters,
                relay_pkgs,
            } => self.service.start_fetch(id, filters, relay_pkgs),
            ScopedSubOutboxOp::UnsubscribeLive { id } => self.service.clear_live(id),
            ScopedSubOutboxOp::ClearFetch { id } => self.service.clear_fetch(id),
            ScopedSubOutboxOp::SetFullHistoryTargets { id, targets } => {
                self.service.set_full_history_targets(id, targets)
            }
            ScopedSubOutboxOp::RemoveFullHistory { id } => self.service.clear_full_history(id),
        }
    }

    fn start_fetch(&mut self, fetch: ResolvedFetch) -> StartedFetch {
        let id = self.service.id_registry().next_sub_id();
        let active = ActiveFetch::new(id, fetch.relays.clone(), fetch.filters.clone());
        let relay_pkgs = RelayUrlPkgs::new(
            fetch.relays,
            RelayUrlPolicy::explicit(
                RelayDemandPriority::Important,
                RelayRoutingPreference::PreferDedicated,
            ),
        );
        let output = self.service.start_fetch(id, fetch.filters, relay_pkgs);
        StartedFetch { active, output }
    }

    fn publish(&mut self, publish: ResolvedPublish) -> OutboxServiceOutput {
        self.service.publish(publish.msg, publish.relays)
    }

    fn set_max_websocket_connections(
        &mut self,
        max_connections: Option<usize>,
    ) -> OutboxServiceOutput {
        self.service.set_max_websocket_connections(max_connections)
    }

    fn clear_fetch(&mut self, id: enostr::OutboxSubId) -> OutboxServiceOutput {
        self.service.clear_fetch(id)
    }
}

struct DirectOperationPlanner {
    publishes: Vec<ResolvedPublish>,
}

impl DirectOperationPlanner {
    fn new() -> Self {
        Self {
            publishes: Vec::new(),
        }
    }

    fn apply_publish(
        &mut self,
        account: Option<&BridgeAccountState>,
        command: RemotePublishCommand,
    ) {
        match command {
            RemotePublishCommand::SelectedAccountWrite { msg } => {
                let account = account.expect("selected account before account-write publish");
                self.publishes.push(ResolvedPublish {
                    msg,
                    relays: account.write_relays().to_vec(),
                });
            }
            RemotePublishCommand::Explicit { msg, relays } => {
                self.publishes.push(ResolvedPublish { msg, relays });
            }
        }
    }

    fn into_publishes(self) -> Vec<ResolvedPublish> {
        self.publishes
    }
}

/// Lightweight bridge event consumed by the UI thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RemoteBridgeEvent {
    Outbox(OutboxEvent),
    ScopedSub(ScopedSubFact),
}

/// Ordered bridge-side work produced while settling scoped-sub runtime output.
enum BridgeSettlementAction {
    Emit(RemoteBridgeEvent),
    StartScopedEffect(ScopedSubEffect),
    ApplyScopedOutboxOps(ScopedSubOutboxOps),
    ClearDirectFetch(enostr::OutboxSubId),
}

/// Thread handle for the remote bridge.
///
/// UI code sends commands into this handle and drains committed read-model facts
/// from it. Remote execution stays on the bridge thread.
pub(crate) struct RemoteBridgeHandle {
    inputs: mpsc::UnboundedSender<RemoteBridgeInput>,
    events: std::sync::mpsc::Receiver<RemoteBridgeEvent>,
    join: Option<std::thread::JoinHandle<()>>,
}

/// Host-facing event sink used by the bridge.
///
/// This owns the handoff from remote facts to UI-thread state: enqueue
/// the fact, then wake the host so `RemoteState::poll_bridge` can apply it.
#[derive(Clone)]
struct RemoteBridgeEventSink {
    events: std::sync::mpsc::Sender<RemoteBridgeEvent>,
    wake_host: Arc<dyn Fn() + Send + Sync>,
}

impl RemoteBridgeEventSink {
    fn new(
        events: std::sync::mpsc::Sender<RemoteBridgeEvent>,
        wake_host: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self { events, wake_host }
    }

    fn send(&self, event: RemoteBridgeEvent) {
        if self.events.send(event).is_ok() {
            (self.wake_host)();
        }
    }
}

impl RemoteBridgeHandle {
    pub(crate) fn spawn(
        ndb: Ndb,
        job_spawner: crate::jobs::JobSpawner,
        wake_host: impl Fn() + Send + Sync + 'static,
        config: RemoteBridgeConfig,
    ) -> Self {
        let (input_sender, input_receiver) = mpsc::unbounded_channel();
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let event_sink = RemoteBridgeEventSink::new(event_sender, Arc::new(wake_host));
        let join = std::thread::Builder::new()
            .name("notedeck-outbox".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("remote bridge tokio runtime");
                runtime.block_on(run_remote_bridge(
                    input_receiver,
                    event_sink,
                    ndb,
                    job_spawner,
                    config,
                ));
            })
            .expect("spawn remote bridge thread");

        Self {
            inputs: input_sender,
            events: event_receiver,
            join: Some(join),
        }
    }

    pub(crate) fn input_sender(&self) -> mpsc::UnboundedSender<RemoteBridgeInput> {
        self.inputs.clone()
    }

    pub(crate) fn send(&self, input: RemoteBridgeInput) {
        if let Err(err) = self.inputs.send(input) {
            tracing::warn!("failed to send remote bridge input: {err}");
        }
    }

    pub(crate) fn drain_events(&mut self, mut handle: impl FnMut(RemoteBridgeEvent)) {
        while let Ok(event) = self.events.try_recv() {
            handle(event);
        }
    }
}

impl Drop for RemoteBridgeHandle {
    fn drop(&mut self) {
        let _ = self.inputs.send(RemoteBridgeInput::Shutdown);
        if let Some(join) = self.join.take() {
            if let Err(err) = join.join() {
                tracing::warn!("remote bridge thread panicked: {err:?}");
            }
        }
    }
}

async fn run_remote_bridge(
    mut bridge_inputs: mpsc::UnboundedReceiver<RemoteBridgeInput>,
    events: RemoteBridgeEventSink,
    ndb: Ndb,
    job_spawner: crate::jobs::JobSpawner,
    config: RemoteBridgeConfig,
) {
    let (input_sender, mut inputs) = mpsc::unbounded_channel();
    let mut actor = RemoteBridge::new(&input_sender, &events, &ndb, &job_spawner, config);
    loop {
        let timer = actor.next_timer();
        let input = tokio::select! {
            input = bridge_inputs.recv() => {
                match input {
                    Some(RemoteBridgeInput::Ui(batch)) => BridgeActorInput::Ui(batch),
                    Some(RemoteBridgeInput::SetMaxWebsocketConnections(max_connections)) => {
                        BridgeActorInput::SetMaxWebsocketConnections(max_connections)
                    }
                    Some(RemoteBridgeInput::Shutdown) => BridgeActorInput::Shutdown,
                    None => BridgeActorInput::Shutdown,
                }
            }
            input = inputs.recv() => {
                input.unwrap_or(BridgeActorInput::Shutdown)
            }
            output = actor.settlement.next() => {
                let actions = actor.settlement.settle_outbox_output(output);
                actor.run_settlement_actions(actions);
                continue;
            }
            _ = wait_for_timer(timer), if timer.is_some() => {
                BridgeActorInput::AuthorOutboxDiscoveryRetryDue
            }
        };

        if actor.apply_input(input) {
            break;
        }
    }
}

struct BridgeOutboxSettlement {
    outbox: BridgeOutboxDriver,
    scoped: ScopedSubRuntime,
    active_fetches: Vec<ActiveFetch>,
}

impl BridgeOutboxSettlement {
    fn new(outbox: BridgeOutboxDriver, scoped: ScopedSubRuntime) -> Self {
        Self {
            outbox,
            scoped,
            active_fetches: Vec::new(),
        }
    }

    fn next_timer(&mut self) -> Option<Instant> {
        self.scoped.next_author_outbox_retry_deadline()
    }

    fn active_fetches(&self) -> &[ActiveFetch] {
        &self.active_fetches
    }

    fn next(&mut self) -> impl Future<Output = OutboxServiceOutput> + '_ {
        self.outbox.next()
    }

    fn apply_scoped_account_initialized(&mut self, pubkey: Pubkey) -> ScopedSubDelta {
        self.scoped.apply_account_initialized(pubkey)
    }

    fn apply_scoped_command(
        &mut self,
        selected_account_pubkey: Pubkey,
        account_read_relays: &HashSet<NormRelayUrl>,
        command: ScopedSubCommand,
    ) -> ScopedSubDelta {
        self.scoped
            .apply_command(selected_account_pubkey, account_read_relays, command)
    }

    fn apply_scoped_account_switched(
        &mut self,
        old_pubkey: Pubkey,
        new_pubkey: Pubkey,
        account_read_relays: &HashSet<NormRelayUrl>,
    ) -> ScopedSubDelta {
        self.scoped
            .apply_account_switched(old_pubkey, new_pubkey, account_read_relays)
    }

    fn apply_scoped_account_read_relays_changed(
        &mut self,
        account_pubkey: Pubkey,
        account_read_relays: &HashSet<NormRelayUrl>,
    ) -> ScopedSubDelta {
        self.scoped
            .apply_account_read_relays_changed(account_pubkey, account_read_relays)
    }

    fn apply_author_outbox_plan_completed(
        &mut self,
        selected_account_pubkey: Pubkey,
        account_read_relays: &HashSet<NormRelayUrl>,
        completion: AuthorOutboxPlanJobCompletion,
    ) -> Vec<BridgeSettlementAction> {
        let delta = self.scoped.apply_author_outbox_plan_completed(
            selected_account_pubkey,
            account_read_relays,
            completion,
        );
        self.settle_scoped_delta(delta)
    }

    fn apply_author_outbox_discovery_retry_due(&mut self) -> Vec<BridgeSettlementAction> {
        let delta = self
            .scoped
            .apply_author_outbox_discovery_retry_due(Instant::now());
        self.settle_scoped_delta(delta)
    }

    fn settle_outbox_output(&mut self, output: OutboxServiceOutput) -> Vec<BridgeSettlementAction> {
        self.settle_outbox_outputs([output])
    }

    fn settle_outbox_outputs(
        &mut self,
        outputs: impl IntoIterator<Item = OutboxServiceOutput>,
    ) -> Vec<BridgeSettlementAction> {
        let mut actions = Vec::new();
        let mut pending_scoped = VecDeque::new();
        for output in outputs {
            pending_scoped.push_back(self.apply_outbox_service_output(output, &mut actions));
        }

        while let Some(scoped_delta) = pending_scoped.pop_front() {
            actions.extend(self.settle_scoped_delta(scoped_delta));
        }

        actions
    }

    fn apply_outbox_service_output(
        &mut self,
        output: OutboxServiceOutput,
        actions: &mut Vec<BridgeSettlementAction>,
    ) -> ScopedSubDelta {
        let OutboxServiceOutput::Events(events) = output else {
            return ScopedSubDelta::default();
        };

        let mut scoped_delta = ScopedSubDelta::default();
        for event in events {
            actions.extend(self.apply_bridge_outbox_fact(event.clone()));
            scoped_delta.extend(self.apply_scoped_outbox_fact(&event));
        }
        scoped_delta
    }

    fn apply_bridge_outbox_fact(&mut self, fact: OutboxEvent) -> Vec<BridgeSettlementAction> {
        let clear_fetches = self.update_active_fetches_from_fact(&fact);
        let mut actions = Vec::with_capacity(1 + clear_fetches.len());
        actions.push(BridgeSettlementAction::Emit(RemoteBridgeEvent::Outbox(
            fact,
        )));
        actions.extend(
            clear_fetches
                .into_iter()
                .map(BridgeSettlementAction::ClearDirectFetch),
        );
        actions
    }

    fn apply_scoped_outbox_fact(&mut self, fact: &OutboxEvent) -> ScopedSubDelta {
        match fact {
            OutboxEvent::OutboxSubRelayEoseChanged { id, relay_eose } => {
                self.scoped.apply_outbox_sub_relay_eose(*id, *relay_eose)
            }
            OutboxEvent::RelayReqStatusChanged { id, relay, status } => self
                .scoped
                .apply_author_outbox_relay_req_status(*id, relay, *status),
            OutboxEvent::RelayStatusChanged { .. } => ScopedSubDelta::default(),
        }
    }

    fn update_active_fetches_from_fact(&mut self, fact: &OutboxEvent) -> Vec<enostr::OutboxSubId> {
        let mut clear_fetches = Vec::new();
        match fact {
            OutboxEvent::OutboxSubRelayEoseChanged {
                id,
                relay_eose: None,
            } => {
                self.active_fetches.retain(|fetch| fetch.id != *id);
            }
            OutboxEvent::RelayReqStatusChanged { id, relay, status } => {
                self.active_fetches.retain_mut(|fetch| {
                    if fetch.id != *id {
                        return true;
                    }

                    if !fetch.apply_relay_req_status(relay, *status) {
                        return true;
                    }

                    if fetch.saw_closed {
                        clear_fetches.push(fetch.id);
                    }
                    false
                });
            }
            _ => {}
        }
        clear_fetches
    }

    fn settle_scoped_delta(&mut self, scoped_delta: ScopedSubDelta) -> Vec<BridgeSettlementAction> {
        let mut actions = Vec::new();
        let mut pending = VecDeque::from([scoped_delta]);
        while let Some(scoped_delta) = pending.pop_front() {
            let (scoped_output, scoped_outbox_ops, scoped_effects) = scoped_delta.into_parts();
            self.append_scoped_output_actions(&mut actions, scoped_output);
            for effect in scoped_effects.into_effects() {
                actions.push(BridgeSettlementAction::StartScopedEffect(effect));
            }
            if !scoped_outbox_ops.is_empty() {
                actions.push(BridgeSettlementAction::ApplyScopedOutboxOps(
                    scoped_outbox_ops,
                ));
            }
        }
        actions
    }

    fn apply_scoped_outbox_ops(
        &mut self,
        scoped_outbox_ops: ScopedSubOutboxOps,
    ) -> Vec<OutboxServiceOutput> {
        self.outbox.apply_scoped_outbox_ops(scoped_outbox_ops)
    }

    fn apply_fetches(&mut self, fetches: Vec<ResolvedFetch>) -> Vec<BridgeSettlementAction> {
        let mut actions = Vec::new();
        for fetch in fetches {
            let output = self.start_fetch(fetch);
            actions.extend(self.settle_outbox_output(output));
        }
        actions
    }

    fn apply_publishes(&mut self, publishes: Vec<ResolvedPublish>) -> Vec<BridgeSettlementAction> {
        let mut actions = Vec::new();
        for publish in publishes {
            let output = self.outbox.publish(publish);
            actions.extend(self.settle_outbox_output(output));
        }
        actions
    }

    fn apply_max_websocket_connections(
        &mut self,
        max_connections: Option<usize>,
    ) -> Vec<BridgeSettlementAction> {
        let output = self.outbox.set_max_websocket_connections(max_connections);
        self.settle_outbox_output(output)
    }

    fn clear_direct_fetch(&mut self, id: enostr::OutboxSubId) -> OutboxServiceOutput {
        self.outbox.clear_fetch(id)
    }

    fn start_fetch(&mut self, fetch: ResolvedFetch) -> OutboxServiceOutput {
        let started = self.outbox.start_fetch(fetch);
        self.active_fetches.push(started.active);
        started.output
    }

    fn append_scoped_output_actions(
        &mut self,
        actions: &mut Vec<BridgeSettlementAction>,
        output: ScopedSubOutput,
    ) {
        if output.is_empty() {
            return;
        }

        for fact in output.into_facts() {
            actions.push(BridgeSettlementAction::Emit(RemoteBridgeEvent::ScopedSub(
                fact,
            )));
        }
    }
}

/// Owns all mutable state for the remote bridge thread.
///
/// UI input mutates bridge policy state. Service output updates bridge read
/// models before facts are forwarded to the host.
struct RemoteBridge<'a> {
    settlement: BridgeOutboxSettlement,
    author_outbox_effects: AuthorOutboxEffectRunner,
    events: &'a RemoteBridgeEventSink,
    accounts: Option<BridgeAccountState>,
}

impl<'a> RemoteBridge<'a> {
    fn new(
        inputs: &'a mpsc::UnboundedSender<BridgeActorInput>,
        events: &'a RemoteBridgeEventSink,
        ndb: &'a Ndb,
        job_spawner: &'a crate::jobs::JobSpawner,
        config: RemoteBridgeConfig,
    ) -> Self {
        let outbox = BridgeOutboxDriver::new(ndb, job_spawner, config);
        let ids = outbox.id_registry();
        let scoped = ScopedSubRuntime::with_ids(ids.clone());
        let author_outbox_effects = AuthorOutboxEffectRunner {
            ndb: ndb.clone(),
            job_spawner: job_spawner.clone(),
            inputs: inputs.clone(),
        };
        let settlement = BridgeOutboxSettlement::new(outbox, scoped);

        Self {
            settlement,
            author_outbox_effects,
            events,
            accounts: None,
        }
    }

    fn next_timer(&mut self) -> Option<Instant> {
        self.settlement.next_timer()
    }

    fn apply_input(&mut self, input: BridgeActorInput) -> bool {
        let actions = match input {
            BridgeActorInput::Ui(batch) => self.apply_ui_batch(batch),
            BridgeActorInput::SetMaxWebsocketConnections(max_connections) => self
                .settlement
                .apply_max_websocket_connections(max_connections),
            BridgeActorInput::AuthorOutboxPlanCompleted(completion) => {
                self.apply_author_outbox_plan_completed(completion)
            }
            BridgeActorInput::AuthorOutboxDiscoveryRetryDue => {
                self.settlement.apply_author_outbox_discovery_retry_due()
            }
            BridgeActorInput::Shutdown => return true,
        };

        self.run_settlement_actions(actions);
        false
    }

    fn run_settlement_actions(&mut self, actions: Vec<BridgeSettlementAction>) {
        let mut pending = VecDeque::from(actions);
        while let Some(action) = pending.pop_front() {
            let next_actions = match action {
                BridgeSettlementAction::Emit(event) => {
                    self.events.send(event);
                    Vec::new()
                }
                BridgeSettlementAction::StartScopedEffect(effect) => {
                    self.author_outbox_effects.apply_effect(effect);
                    Vec::new()
                }
                BridgeSettlementAction::ApplyScopedOutboxOps(scoped_outbox_ops) => {
                    let outputs = self.settlement.apply_scoped_outbox_ops(scoped_outbox_ops);
                    self.settlement.settle_outbox_outputs(outputs)
                }
                BridgeSettlementAction::ClearDirectFetch(id) => {
                    let output = self.settlement.clear_direct_fetch(id);
                    self.settlement.settle_outbox_output(output)
                }
            };

            for action in next_actions.into_iter().rev() {
                pending.push_front(action);
            }
        }
    }

    fn apply_ui_batch(&mut self, batch: RemoteIntentBatch) -> Vec<BridgeSettlementAction> {
        let mut fetch_planner = FetchPlanner::new();
        let mut direct_planner = DirectOperationPlanner::new();
        let mut scoped_delta = ScopedSubDelta::default();

        for section in batch.sections {
            if let Some(account) = section.account_changed {
                scoped_delta.extend(self.apply_account_changed(account));
            }

            for intent in section.intents {
                match intent {
                    RemoteIntent::ScopedSub(command) => {
                        scoped_delta.extend(self.apply_scoped_sub_command(command));
                    }
                    RemoteIntent::Fetch(command) => {
                        let account = self.account_state();
                        fetch_planner.apply_fetch(account, command);
                    }
                    RemoteIntent::Publish(command) => {
                        direct_planner.apply_publish(self.accounts.as_ref(), command);
                    }
                }
            }
        }

        let fetches = fetch_planner.into_fetches(self.settlement.active_fetches());
        let publishes = direct_planner.into_publishes();
        let mut actions = self.settlement.settle_scoped_delta(scoped_delta);
        actions.extend(self.settlement.apply_fetches(fetches));
        actions.extend(self.settlement.apply_publishes(publishes));
        actions
    }

    fn account_state(&self) -> &BridgeAccountState {
        self.accounts
            .as_ref()
            .expect("selected account before selected-account remote work")
    }

    fn apply_account_changed(&mut self, account: BridgeAccountState) -> ScopedSubDelta {
        let previous = self.accounts.replace(account);
        let new_pubkey = self.account_state().selected_pubkey();
        let new_read_relays = self.account_state().read_relays().clone();
        match previous {
            Some(previous) if previous.selected_pubkey() != new_pubkey => {
                self.apply_scoped_account_switched(previous.selected_pubkey(), new_pubkey)
            }
            Some(previous) if previous.read_relays() != &new_read_relays => {
                self.apply_scoped_account_read_relays_changed(new_pubkey)
            }
            None => self.apply_scoped_account_initialized(new_pubkey),
            _ => ScopedSubDelta::default(),
        }
    }

    /// Accept scoped-sub declaration commands at the bridge boundary.
    fn apply_scoped_sub_command(&mut self, command: ScopedSubCommand) -> ScopedSubDelta {
        self.apply_scoped_declaration_command(command)
    }

    fn apply_scoped_declaration_command(&mut self, command: ScopedSubCommand) -> ScopedSubDelta {
        let account = self.account_state();
        let selected_account_pubkey = account.selected_pubkey();
        let account_read_relays = account.read_relays().clone();
        self.settlement
            .apply_scoped_command(selected_account_pubkey, &account_read_relays, command)
    }

    fn apply_scoped_account_switched(
        &mut self,
        old_pubkey: Pubkey,
        new_pubkey: Pubkey,
    ) -> ScopedSubDelta {
        let account_read_relays = self.account_state().read_relays().clone();
        self.settlement
            .apply_scoped_account_switched(old_pubkey, new_pubkey, &account_read_relays)
    }

    fn apply_scoped_account_read_relays_changed(
        &mut self,
        account_pubkey: Pubkey,
    ) -> ScopedSubDelta {
        let account_read_relays = self.account_state().read_relays().clone();
        self.settlement
            .apply_scoped_account_read_relays_changed(account_pubkey, &account_read_relays)
    }

    fn apply_scoped_account_initialized(&mut self, pubkey: Pubkey) -> ScopedSubDelta {
        self.settlement.apply_scoped_account_initialized(pubkey)
    }

    fn apply_author_outbox_plan_completed(
        &mut self,
        completion: AuthorOutboxPlanJobCompletion,
    ) -> Vec<BridgeSettlementAction> {
        let account = self.account_state();
        let selected_account_pubkey = account.selected_pubkey();
        let account_read_relays = account.read_relays().clone();
        self.settlement.apply_author_outbox_plan_completed(
            selected_account_pubkey,
            &account_read_relays,
            completion,
        )
    }
}

/// Result of one ndb note-id presence snapshot.
struct NotePresence {
    present_ids: HashSet<NoteId>,
    missing_ids: HashSet<NoteId>,
}

/// Snapshot note-id presence with one short ndb transaction.
fn note_presence(ndb: &Ndb, candidate_ids: HashSet<NoteId>) -> NotePresence {
    let Ok(txn) = Transaction::new(ndb) else {
        tracing::warn!("full-history note presence check skipped: failed to open txn");
        return NotePresence {
            present_ids: HashSet::new(),
            missing_ids: candidate_ids,
        };
    };

    let mut missing_ids = HashSet::new();
    let mut present_ids = HashSet::new();
    for id in candidate_ids {
        if ndb.get_note_by_id(&txn, id.bytes()).is_ok() {
            present_ids.insert(id);
        } else {
            missing_ids.insert(id);
        }
    }

    NotePresence {
        present_ids,
        missing_ids,
    }
}

fn full_history_local_presence_result(
    ndb: &Ndb,
    request_id: u64,
    candidate_ids: HashSet<NoteId>,
) -> FullHistoryLocalPresenceResult {
    let presence = note_presence(ndb, candidate_ids);
    FullHistoryLocalPresenceResult {
        request_id,
        missing_ids: presence.missing_ids,
        already_local_ids: presence.present_ids,
    }
}

fn process_relay_event_ingest(ndb: &Ndb, request: EventIngestRequest) {
    let from_client = match request.relay_type {
        RelayImplType::Websocket => false,
        RelayImplType::Multicast => true,
    };

    profiling::scope!("ndb process event");
    match ndb.process_event_with(
        &request.ingest_json,
        nostrdb::IngestMetadata::new()
            .client(from_client)
            .relay(&request.relay_url),
    ) {
        Ok(_) => {}
        Err(err) => {
            tracing::error!("error processing event {}: {err}", request.ingest_json);
        }
    }
}

/// Poll ndb for fetched-event visibility until a short pre-timeout final
/// snapshot.
async fn pending_ingestion_presence_result(
    ndb: Ndb,
    request: FullHistoryPendingIngestionPresenceRequest,
) -> FullHistoryPendingIngestionPresenceResult {
    let mut missing_ids = request.candidate_ids;
    let mut stored_ids = HashSet::new();
    let final_check_at = request
        .deadline
        .checked_sub(PENDING_INGESTION_PRESENCE_FINAL_CHECK_LEAD)
        .unwrap_or(request.deadline);
    let mut attempt = 0;

    loop {
        let presence = note_presence(&ndb, missing_ids);
        stored_ids.extend(presence.present_ids);
        missing_ids = presence.missing_ids;
        if missing_ids.is_empty() {
            break;
        }

        let now = Instant::now();
        if now >= final_check_at {
            break;
        }

        let sleep_for = pending_ingestion_presence_backoff(attempt)
            .min(final_check_at.saturating_duration_since(now));
        attempt += 1;
        if sleep_for.is_zero() {
            break;
        }
        tokio::time::sleep(sleep_for).await;
    }

    FullHistoryPendingIngestionPresenceResult { stored_ids }
}

fn pending_ingestion_presence_backoff(attempt: u32) -> Duration {
    let multiplier = 1u32.checked_shl(attempt.min(8)).unwrap_or(u32::MAX);
    PENDING_INGESTION_PRESENCE_BACKOFF_BASE
        .saturating_mul(multiplier)
        .min(PENDING_INGESTION_PRESENCE_BACKOFF_MAX)
}

async fn wait_for_timer(timer: Option<Instant>) -> Option<Instant> {
    let Some(deadline) = timer else {
        std::future::pending::<()>().await;
        return None;
    };
    tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
    Some(deadline)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        test_utils::{nip65_write_relay_note_for_test, wait_for_nip65_for_test},
        SubConfig, SubKey, SubOwnerKey, SubRelayPolicy, SubScope,
    };
    use enostr::{FullKeypair, RelayStatus};
    use nostrdb::Config;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use tempfile::TempDir;

    fn relay() -> NormRelayUrl {
        NormRelayUrl::new("wss://relay-nip11-bridge.example.com").expect("relay")
    }

    fn test_ndb() -> (TempDir, Ndb) {
        let tmp = TempDir::new().expect("tmp dir");
        let ndb = Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
        (tmp, ndb)
    }

    fn test_settlement(
        active_fetches: Vec<ActiveFetch>,
    ) -> (TempDir, crate::jobs::JobPool, BridgeOutboxSettlement) {
        crate::app::install_crypto();
        let (tmp, ndb) = test_ndb();
        let job_pool = crate::jobs::JobPool::new(1);
        let job_spawner = job_pool.spawner();
        let outbox = BridgeOutboxDriver::new(&ndb, &job_spawner, RemoteBridgeConfig::default());
        let ids = outbox.id_registry();
        (
            tmp,
            job_pool,
            BridgeOutboxSettlement {
                outbox,
                scoped: ScopedSubRuntime::with_ids(ids),
                active_fetches,
            },
        )
    }

    fn author_outbox_config(author: Pubkey) -> SubConfig {
        let baseline = SubRelayPolicy::new(
            RelayDemandPriority::Important,
            RelayRoutingPreference::PreferDedicated,
        );
        let author_outbox = SubRelayPolicy::new(
            RelayDemandPriority::Opportunistic,
            RelayRoutingPreference::NoPreference,
        );

        SubConfig::builder(vec![Filter::new()
            .authors([author.bytes()])
            .kinds([1])
            .limit(20)
            .build()])
        .accounts_read(baseline)
        .with_author_outbox(author_outbox)
        .build()
    }

    fn drain_bridge_events(
        receiver: &std::sync::mpsc::Receiver<RemoteBridgeEvent>,
    ) -> Vec<RemoteBridgeEvent> {
        receiver.try_iter().collect()
    }

    #[test]
    fn bridge_event_shape_is_snapshot_facts_only() {
        let event = RemoteBridgeEvent::Outbox(OutboxEvent::RelayStatusChanged {
            relay: relay(),
            status: None,
        });

        match event {
            RemoteBridgeEvent::Outbox(OutboxEvent::RelayStatusChanged { status, .. }) => {
                assert_eq!(status, None)
            }
            _ => panic!("expected relay status fact"),
        }
    }

    #[test]
    fn unrelated_outbox_event_does_not_emit_scoped_facts() {
        crate::app::install_crypto();
        let (_tmp, ndb) = test_ndb();
        let author = FullKeypair::generate();
        let author_relay = "wss://author-route.example.com";
        let note = nip65_write_relay_note_for_test(&author, &[author_relay]);
        ndb.process_client_event(&note.json().expect("nip65 json"))
            .expect("ingest nip65");
        wait_for_nip65_for_test(&ndb, &author.pubkey);

        let (input_sender, _input_receiver) = mpsc::unbounded_channel();
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let event_sink = RemoteBridgeEventSink::new(event_sender, Arc::new(|| {}));
        let job_pool = crate::jobs::JobPool::new(1);
        let job_spawner = job_pool.spawner();
        let mut bridge = RemoteBridge::new(
            &input_sender,
            &event_sink,
            &ndb,
            &job_spawner,
            RemoteBridgeConfig::default(),
        );
        let selected_account_pubkey = Pubkey::new([0x01; 32]);
        let account_read_relay =
            NormRelayUrl::new("wss://account-read.example.com").expect("relay");
        bridge.accounts = Some(BridgeAccountState::new(
            selected_account_pubkey,
            HashSet::from([account_read_relay]),
            Vec::new(),
        ));

        let owner = SubOwnerKey::new("unrelated-outbox-event-owner");
        let key = SubKey::new("unrelated-outbox-event-key");
        let config = author_outbox_config(author.pubkey);
        let command = ScopedSubCommand::set_owner_config(
            selected_account_pubkey,
            owner,
            SubScope::Global,
            key,
            config,
        );
        let delta = bridge.apply_scoped_declaration_command(command);
        let actions = bridge.settlement.settle_scoped_delta(delta);
        bridge.run_settlement_actions(actions);
        let _ = drain_bridge_events(&event_receiver);

        let unrelated = OutboxEvent::RelayStatusChanged {
            relay: relay(),
            status: Some(RelayStatus::Connected),
        };
        let actions = bridge
            .settlement
            .settle_outbox_output(OutboxServiceOutput::Events(vec![unrelated.clone()]));
        bridge.run_settlement_actions(actions);

        assert_eq!(
            drain_bridge_events(&event_receiver),
            vec![RemoteBridgeEvent::Outbox(unrelated)]
        );
    }

    #[test]
    fn bridge_event_sink_queues_fact_and_wakes_host() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let wake_count = Arc::new(AtomicUsize::new(0));
        let wake_count_for_sink = Arc::clone(&wake_count);
        let sink = RemoteBridgeEventSink::new(
            sender,
            Arc::new(move || {
                wake_count_for_sink.fetch_add(1, AtomicOrdering::SeqCst);
            }),
        );
        let event = RemoteBridgeEvent::Outbox(OutboxEvent::RelayStatusChanged {
            relay: relay(),
            status: Some(enostr::RelayStatus::Connected),
        });

        sink.send(event.clone());

        assert_eq!(receiver.try_recv().expect("queued event"), event);
        assert_eq!(wake_count.load(AtomicOrdering::SeqCst), 1);
    }

    #[test]
    fn max_websocket_limit_control_does_not_require_selected_account() {
        crate::app::install_crypto();
        let (_tmp, ndb) = test_ndb();
        let (input_sender, _input_receiver) = mpsc::unbounded_channel();
        let (event_sender, _event_receiver) = std::sync::mpsc::channel();
        let event_sink = RemoteBridgeEventSink::new(event_sender, Arc::new(|| {}));
        let job_pool = crate::jobs::JobPool::new(1);
        let job_spawner = job_pool.spawner();
        let mut bridge = RemoteBridge::new(
            &input_sender,
            &event_sink,
            &ndb,
            &job_spawner,
            RemoteBridgeConfig::default(),
        );

        assert!(!bridge.apply_input(BridgeActorInput::SetMaxWebsocketConnections(Some(0))));
        assert!(bridge.accounts.is_none());
    }

    #[test]
    fn fetch_planner_suppresses_duplicate_active_fetch() {
        let relay = relay();
        let relays = HashSet::from([relay]);
        let filters = vec![Filter::new().kinds([1]).build()];
        let active_fetches = vec![ActiveFetch::new(
            enostr::OutboxSubId(1),
            relays.clone(),
            filters.clone(),
        )];
        let account = BridgeAccountState::new(Pubkey::new([0x01; 32]), relays, Vec::new());
        let mut planner = FetchPlanner::new();

        planner.add_selected_account_read(&account, filters);

        assert!(planner.into_fetches(&active_fetches).is_empty());
    }

    #[test]
    fn direct_fetch_closed_status_releases_active_dedupe() {
        let relay = relay();
        let id = enostr::OutboxSubId(1);
        let (_tmp, _job_pool, mut settlement) = test_settlement(vec![ActiveFetch::new(
            id,
            HashSet::from([relay.clone()]),
            vec![Filter::new().kinds([1]).build()],
        )]);

        let clear_fetches =
            settlement.update_active_fetches_from_fact(&OutboxEvent::RelayReqStatusChanged {
                id,
                relay,
                status: Some(RelayReqStatus::Closed),
            });

        assert_eq!(clear_fetches, vec![id]);
        assert!(settlement.active_fetches.is_empty());
    }

    #[test]
    fn direct_fetch_closed_status_waits_for_other_pending_relays() {
        let relay_a = relay();
        let relay_b = NormRelayUrl::new("wss://direct-fetch-b.example.com").expect("relay");
        let id = enostr::OutboxSubId(1);
        let (_tmp, _job_pool, mut settlement) = test_settlement(vec![ActiveFetch::new(
            id,
            HashSet::from([relay_a.clone(), relay_b.clone()]),
            vec![Filter::new().kinds([1]).build()],
        )]);

        let clear_fetches =
            settlement.update_active_fetches_from_fact(&OutboxEvent::RelayReqStatusChanged {
                id,
                relay: relay_a,
                status: Some(RelayReqStatus::Closed),
            });

        assert!(clear_fetches.is_empty());
        assert_eq!(settlement.active_fetches.len(), 1);

        let clear_fetches =
            settlement.update_active_fetches_from_fact(&OutboxEvent::RelayReqStatusChanged {
                id,
                relay: relay_b,
                status: Some(RelayReqStatus::Eose),
            });

        assert_eq!(clear_fetches, vec![id]);
        assert!(settlement.active_fetches.is_empty());
    }

    #[test]
    fn direct_fetch_success_cleanup_releases_active_dedupe_without_clear() {
        let relay = relay();
        let id = enostr::OutboxSubId(1);
        let (_tmp, _job_pool, mut settlement) = test_settlement(vec![ActiveFetch::new(
            id,
            HashSet::from([relay]),
            vec![Filter::new().kinds([1]).build()],
        )]);

        let clear_fetches =
            settlement.update_active_fetches_from_fact(&OutboxEvent::OutboxSubRelayEoseChanged {
                id,
                relay_eose: None,
            });

        assert!(clear_fetches.is_empty());
        assert!(settlement.active_fetches.is_empty());
    }

    #[test]
    fn fetch_planner_drops_selected_account_fetch_without_relays() {
        let account = BridgeAccountState::new(Pubkey::new([0x01; 32]), HashSet::new(), Vec::new());
        let filters = vec![Filter::new().kinds([1]).build()];
        let mut planner = FetchPlanner::new();

        planner.add_selected_account_read(&account, filters);

        assert!(planner.into_fetches(&[]).is_empty());
    }

    #[test]
    fn fetch_planner_suppresses_duplicate_batch_fetch() {
        let relay = relay();
        let relays = HashSet::from([relay]);
        let filters = vec![Filter::new().kinds([1]).build()];
        let account = BridgeAccountState::new(Pubkey::new([0x01; 32]), relays, Vec::new());
        let mut planner = FetchPlanner::new();

        planner.add_selected_account_read(&account, filters.clone());
        planner.add_selected_account_read(&account, filters);

        assert_eq!(planner.into_fetches(&[]).len(), 1);
    }
}
