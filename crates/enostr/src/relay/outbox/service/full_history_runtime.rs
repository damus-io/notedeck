use std::time::Instant;

use hashbrown::{HashMap, HashSet};
use nostrdb::Filter;

use super::{
    EventIngestCapability, FullHistoryCapability, FullHistoryLocalSetResult, Nip11Capability,
    OutboxService,
};
use crate::relay::coordinator::{FullHistoryNegentropyCapacityGrant, NegentropyCapacityError};
use crate::relay::frame::{Framed, QueuedRelayFrame, RelayFrameSink};
use crate::relay::negentropy::{
    ActiveSessionRelayDemand, NegentropyNeed, NegentropyRelay, NegentropyRelayEffect,
    NegentropyRelayEffects, NegentropyRetry, NegentropyStartResult, RelayTimeoutDeadline,
};
use crate::relay::outbox::full_history::{
    full_history_snapshot_from_task, FullHistoryNeed, FullHistoryNegentropyStartOutcome,
    FullHistorySnapshot, FullHistoryUpsert, LocalPresenceStage,
};
use crate::relay::outbox::{
    FullHistoryOutput, OutboxPool, OutboxPoolOutput, OutboxServiceOutput, OutboxTransportEffect,
    RelayConnectionDropReason, RelayTransportDemand,
};
use crate::relay::{
    subscription::FullHistoryTask, FullHistoryLocalPresenceResult,
    FullHistoryPendingIngestionPresenceResult, FullHistorySubId, Nip11Applied, Nip11LimitationsRaw,
    NormRelayUrl, RelayLimitations, RelayUrlSource, SubPass,
};

/// What applying full-history tasks produced, and the relays they stopped
/// using, whose websockets may now be idle.
pub(in crate::relay::outbox) struct FullHistoryTaskOutput {
    pub(in crate::relay::outbox) idle_websocket_eviction_candidates: HashSet<NormRelayUrl>,
    pub(in crate::relay::outbox) output: FullHistoryRuntimeOutput,
}

impl FullHistoryTaskOutput {
    fn empty() -> Self {
        Self {
            idle_websocket_eviction_candidates: HashSet::new(),
            output: FullHistoryRuntimeOutput::empty(),
        }
    }

    fn extend(&mut self, other: Self) {
        self.idle_websocket_eviction_candidates
            .extend(other.idle_websocket_eviction_candidates);
        self.output.extend(other.output);
    }
}

/// What a relay-local negentropy step leaves for the service to finish:
/// transport work, the missing ids it surfaced, and the full-history retries
/// it scheduled.
#[derive(Default)]
struct NegentropyRelease {
    pool: OutboxPoolOutput,
    surfaced_needs: Vec<FullHistoryNeed>,
    full_history: FullHistoryOutput,
}

impl NegentropyRelease {
    /// Starts a release that sends the frames a step queued on `relay`.
    fn from_frames(relay: &NormRelayUrl, frames: Vec<QueuedRelayFrame>) -> Self {
        let mut release = Self::default();
        release
            .pool
            .transport_effects
            .extend(OutboxPool::relay_frame_effects(relay, frames));
        release
    }

    fn extend(&mut self, other: Self) {
        self.pool.extend(other.pool);
        self.surfaced_needs.extend(other.surfaced_needs);
        self.full_history.extend(other.full_history);
    }
}

pub(in crate::relay::outbox) struct FullHistoryRuntimeOutput {
    pub(in crate::relay::outbox) full_history: FullHistoryOutput,
    pub(in crate::relay::outbox) negentropy_demand_changes:
        HashMap<NormRelayUrl, Option<RelayTransportDemand>>,
    pub(in crate::relay::outbox) pool: OutboxPoolOutput,
}

struct ServiceNegentropyOutput {
    full_history: FullHistoryOutput,
    pool: OutboxPoolOutput,
    negentropy_demand_changes: HashMap<NormRelayUrl, Option<RelayTransportDemand>>,
}

impl ServiceNegentropyOutput {
    fn empty() -> Self {
        Self {
            full_history: FullHistoryOutput::default(),
            pool: OutboxPoolOutput::default(),
            negentropy_demand_changes: HashMap::new(),
        }
    }

    fn into_full_history_runtime_output(self) -> FullHistoryRuntimeOutput {
        FullHistoryRuntimeOutput {
            full_history: self.full_history,
            negentropy_demand_changes: self.negentropy_demand_changes,
            pool: self.pool,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct FullHistoryRuntimeDeadline {
    pub(super) deadline: Instant,
    pub(super) input: FullHistoryRuntimeDeadlineInput,
}

#[derive(Clone, Debug)]
pub(super) enum FullHistoryRuntimeDeadlineInput {
    Workflow,
    NegentropyTimeout { relay: NormRelayUrl },
}

impl FullHistoryRuntimeOutput {
    pub(in crate::relay::outbox) fn empty() -> Self {
        Self {
            full_history: FullHistoryOutput::default(),
            negentropy_demand_changes: HashMap::new(),
            pool: OutboxPoolOutput::default(),
        }
    }

    fn from_full_history(full_history: FullHistoryOutput) -> Self {
        Self {
            full_history,
            negentropy_demand_changes: HashMap::new(),
            pool: OutboxPoolOutput::default(),
        }
    }

    fn extend(&mut self, output: FullHistoryRuntimeOutput) {
        self.full_history.extend(output.full_history);
        self.negentropy_demand_changes
            .extend(output.negentropy_demand_changes);
        self.pool.extend(output.pool);
    }
}

impl<N, F, E> OutboxService<N, F, E>
where
    N: Nip11Capability<Output = Result<Nip11LimitationsRaw, String>>,
    F: FullHistoryCapability<
        LocalSetOutput = FullHistoryLocalSetResult,
        LocalPresenceOutput = FullHistoryLocalPresenceResult,
        PendingIngestionPresenceOutput = FullHistoryPendingIngestionPresenceResult,
    >,
    E: EventIngestCapability,
{
    /// Return the next service-owned full-history workflow deadline.
    pub(super) fn next_full_history_runtime_deadline(&self) -> Option<FullHistoryRuntimeDeadline> {
        let now = Instant::now();
        let workflow =
            self.full_history
                .next_deadline(now)
                .map(|deadline| FullHistoryRuntimeDeadline {
                    deadline,
                    input: FullHistoryRuntimeDeadlineInput::Workflow,
                });
        let negentropy =
            self.next_negentropy_timeout_deadline()
                .map(|timeout| FullHistoryRuntimeDeadline {
                    deadline: timeout.deadline,
                    input: FullHistoryRuntimeDeadlineInput::NegentropyTimeout {
                        relay: timeout.relay,
                    },
                });
        match (workflow, negentropy) {
            (Some(workflow), Some(negentropy)) if negentropy.deadline < workflow.deadline => {
                Some(negentropy)
            }
            (Some(workflow), Some(_)) | (Some(workflow), None) => Some(workflow),
            (None, Some(negentropy)) => Some(negentropy),
            (None, None) => None,
        }
    }

    fn next_negentropy_timeout_deadline(&self) -> Option<RelayTimeoutDeadline> {
        self.negentropy
            .next_timeout_deadline_matching(|relay| self.relay.transport.subids_supported(relay))
    }

    fn apply_negentropy_effect_with_service_runtime(
        &mut self,
        relay: &NormRelayUrl,
        effect: NegentropyRelayEffect,
    ) -> ServiceNegentropyOutput {
        let release = match effect {
            NegentropyRelayEffect::RevocateSessions {
                generation,
                revocations,
            } => {
                let Framed {
                    value: mut revocation_effects,
                    frames,
                } = {
                    let negentropy = self.negentropy.relay_mut(relay);
                    NegentropyRelay::run_framed(generation, negentropy, |relay| {
                        relay.revocate_sessions(revocations.len())
                    })
                };
                debug_assert_eq!(
                    revocation_effects.revoked_passes.len(),
                    revocations.len(),
                    "coordinator selected more negentropy revocations than active sessions"
                );
                for (mut revocation, pass) in revocations
                    .into_iter()
                    .zip(std::mem::take(&mut revocation_effects.revoked_passes))
                {
                    revocation.revocate(pass);
                }
                let mut release = NegentropyRelease::from_frames(relay, frames);
                release.full_history = self.schedule_negentropy_retries_for_relay(
                    relay,
                    revocation_effects.take_retries(),
                    Instant::now(),
                );
                release
            }
            NegentropyRelayEffect::RelayDisconnect => {
                let effects = {
                    let negentropy = self.negentropy.relay_mut(relay);
                    NegentropyRelay::new(RelayFrameSink::disconnected(), negentropy)
                        .handle_relay_disconnect()
                };
                self.apply_negentropy_effects_after_release(relay, effects, Instant::now())
            }
            NegentropyRelayEffect::Timeout { generation, now } => {
                let step = {
                    let negentropy = self.negentropy.relay_mut(relay);
                    NegentropyRelay::run_framed(generation, negentropy, |relay| {
                        relay.handle_timeout(now)
                    })
                };
                self.release_framed_negentropy_step(relay, step, now)
            }
            NegentropyRelayEffect::CancelOwner {
                generation,
                owner_history_id,
            } => {
                let step = {
                    let negentropy = self.negentropy.relay_mut(relay);
                    NegentropyRelay::run_framed(generation, negentropy, |relay| {
                        relay.cancel_owner(owner_history_id)
                    })
                };
                self.release_framed_negentropy_step(relay, step, Instant::now())
            }
            NegentropyRelayEffect::CancelOwnerFilters {
                generation,
                owner_history_id,
                filters,
            } => {
                let step = {
                    let negentropy = self.negentropy.relay_mut(relay);
                    NegentropyRelay::run_framed(generation, negentropy, |relay| {
                        relay.cancel_owner_filters(owner_history_id, &filters)
                    })
                };
                self.release_framed_negentropy_step(relay, step, Instant::now())
            }
            NegentropyRelayEffect::DropSessionsWithoutNegClose => {
                let effects = {
                    let negentropy = self.negentropy.relay_mut(relay);
                    NegentropyRelay::new(RelayFrameSink::disconnected(), negentropy)
                        .drop_sessions_without_neg_close()
                };
                self.apply_negentropy_effects_after_release(relay, effects, Instant::now())
            }
        };

        self.settle_negentropy_release(relay, release)
    }

    /// Stages the fetches a negentropy release surfaced, records the relay's
    /// new negentropy demand, and asks for capacity for any ready pending set.
    fn settle_negentropy_release(
        &mut self,
        relay: &NormRelayUrl,
        release: NegentropyRelease,
    ) -> ServiceNegentropyOutput {
        let NegentropyRelease {
            mut pool,
            surfaced_needs,
            mut full_history,
        } = release;
        let mut negentropy_demand_changes =
            HashMap::from([(relay.clone(), self.negentropy_transport_demand_for(relay))]);
        let followup = self.stage_full_history_fetches(surfaced_needs);
        negentropy_demand_changes.extend(followup.negentropy_demand_changes);
        pool.extend(followup.pool);
        full_history.extend(followup.full_history);

        pool.extend(self.request_pending_full_history_negentropy_capacity(relay));
        ServiceNegentropyOutput {
            full_history,
            pool,
            negentropy_demand_changes,
        }
    }

    /// Sends a relay-local negentropy step's frames and releases its effects.
    fn release_framed_negentropy_step(
        &mut self,
        relay: &NormRelayUrl,
        step: Framed<NegentropyRelayEffects>,
        now: Instant,
    ) -> NegentropyRelease {
        let mut release = NegentropyRelease::from_frames(relay, step.frames);
        release.extend(self.apply_negentropy_effects_after_release(relay, step.value, now));
        release
    }

    /// Finishes a negentropy step run for one relay message, all the way to
    /// service output.
    fn finish_relay_message_negentropy_step(
        &mut self,
        relay: &NormRelayUrl,
        step: Framed<NegentropyRelayEffects>,
    ) -> OutboxServiceOutput {
        let release = self.release_framed_negentropy_step(relay, step, Instant::now());
        let output = self.settle_negentropy_release(relay, release);
        self.handle_full_history_output(output.into_full_history_runtime_output())
    }

    pub(in crate::relay::outbox) fn apply_negentropy_effect(
        &mut self,
        relay: &NormRelayUrl,
        effect: NegentropyRelayEffect,
    ) -> FullHistoryRuntimeOutput {
        self.apply_negentropy_effect_with_service_runtime(relay, effect)
            .into_full_history_runtime_output()
    }

    pub(super) fn apply_relay_connection_eviction(
        &mut self,
        relay: &NormRelayUrl,
        reason: RelayConnectionDropReason,
    ) -> OutboxServiceOutput {
        let output = self.pool.evict_relay_connection_for_reason(relay, reason);
        self.handle_pool_output(output)
    }

    pub(in crate::relay::outbox) fn apply_relay_transport_opened(
        &mut self,
        relay: NormRelayUrl,
        generation: u64,
    ) -> OutboxServiceOutput {
        let mut output = self
            .pool
            .apply_relay_transport_opened(relay.clone(), generation);
        output.extend(self.request_pending_full_history_negentropy_capacity(&relay));
        self.handle_full_history_output(FullHistoryRuntimeOutput {
            full_history: FullHistoryOutput::default(),
            negentropy_demand_changes: HashMap::new(),
            pool: output,
        })
    }

    pub(in crate::relay::outbox) fn apply_relay_transport_closed(
        &mut self,
        relay: &NormRelayUrl,
        generation: u64,
        now: Instant,
    ) -> OutboxServiceOutput {
        let output = self
            .pool
            .apply_relay_transport_closed(relay, generation, now);
        self.handle_pool_output(output)
    }

    pub(in crate::relay::outbox) fn apply_relay_transport_error(
        &mut self,
        relay: &NormRelayUrl,
        generation: u64,
        error: String,
        now: Instant,
    ) -> OutboxServiceOutput {
        let output = self
            .pool
            .apply_relay_transport_error(relay, generation, error, now);
        self.handle_pool_output(output)
    }

    pub(in crate::relay::outbox) fn apply_relay_notice(
        &mut self,
        relay: &NormRelayUrl,
        generation: u64,
        notice: &str,
    ) -> OutboxServiceOutput {
        tracing::warn!("Notice from {}: {}", relay, notice);
        let step = {
            let negentropy = self.negentropy.relay_mut(relay);
            NegentropyRelay::run_framed(Some(generation), negentropy, |relay| {
                relay.handle_notice(notice)
            })
        };
        self.finish_relay_message_negentropy_step(relay, step)
    }

    pub(in crate::relay::outbox) fn apply_relay_neg_msg(
        &mut self,
        relay: &NormRelayUrl,
        generation: u64,
        sub_id: &str,
        payload: &str,
    ) -> OutboxServiceOutput {
        let step = {
            let negentropy = self.negentropy.relay_mut(relay);
            NegentropyRelay::run_framed(Some(generation), negentropy, |relay| {
                relay.handle_neg_msg(sub_id, payload)
            })
        };
        self.finish_relay_message_negentropy_step(relay, step)
    }

    pub(in crate::relay::outbox) fn apply_relay_neg_err(
        &mut self,
        relay: &NormRelayUrl,
        generation: u64,
        sub_id: &str,
        reason: &str,
    ) -> OutboxServiceOutput {
        let step = {
            let negentropy = self.negentropy.relay_mut(relay);
            NegentropyRelay::run_framed(Some(generation), negentropy, |relay| {
                relay.handle_neg_err(sub_id, reason)
            })
        };
        self.finish_relay_message_negentropy_step(relay, step)
    }

    pub(in crate::relay::outbox) fn apply_unsupported_subid_length(
        &mut self,
        relay: &NormRelayUrl,
        max_subid_length: usize,
    ) -> Nip11Applied<OutboxServiceOutput> {
        let applied = self
            .pool
            .apply_unsupported_subid_length(relay, max_subid_length);
        let mut service_output = self.handle_pool_output(applied.output);
        if self.relay.transport.set_subids_supported(relay, false) {
            service_output = super::merge_service_outputs(
                service_output,
                self.evict_idle_websockets_after_unsubscribes(HashSet::from([relay.clone()])),
            );
        }
        Nip11Applied {
            outcome: applied.outcome,
            output: service_output,
        }
    }

    pub(in crate::relay::outbox) fn apply_relay_limit_update(
        &mut self,
        relay: &NormRelayUrl,
        limitations: RelayLimitations,
    ) -> Nip11Applied<OutboxServiceOutput> {
        let active_negentropy_session_count = self
            .negentropy
            .relay(relay)
            .map(|data| data.active_session_count())
            .unwrap_or_default();
        let applied =
            self.pool
                .apply_relay_limit_update(relay, limitations, active_negentropy_session_count);
        Nip11Applied {
            outcome: applied.outcome,
            output: self.handle_pool_output(applied.output),
        }
    }

    pub(in crate::relay::outbox) fn apply_full_history_tasks(
        &mut self,
        tasks: HashMap<FullHistorySubId, FullHistoryTask>,
    ) -> FullHistoryTaskOutput {
        let mut output = FullHistoryTaskOutput::empty();

        for (id, task) in tasks {
            match task {
                FullHistoryTask::Upsert(task) => {
                    let relay_pkgs = task.relay_pkgs();
                    output.extend(
                        self.upsert_full_history_snapshot(full_history_snapshot_from_task(
                            id, &task,
                        )),
                    );
                    for relay_pkgs in relay_pkgs {
                        for relay in relay_pkgs.urls {
                            if !self.relay.transport.subids_supported(&relay) {
                                continue;
                            }

                            self.pool.ensure_relay(&relay);
                        }
                    }
                }
                FullHistoryTask::Remove => {
                    output.extend(self.remove_full_history_sub(id));
                }
            }
        }

        output
    }

    pub(in crate::relay::outbox) fn apply_full_history_negentropy_capacity_grant(
        &mut self,
        relay: NormRelayUrl,
        grant: FullHistoryNegentropyCapacityGrant,
    ) -> FullHistoryRuntimeOutput {
        self.advance_pending_full_history_neg_sets_with_grant(relay, grant)
    }

    fn advance_pending_full_history_neg_sets_with_grant(
        &mut self,
        relay: NormRelayUrl,
        grant: FullHistoryNegentropyCapacityGrant,
    ) -> FullHistoryRuntimeOutput {
        let mut grant = Some(grant);
        let mut returned_passes = Vec::<(NormRelayUrl, SubPass)>::new();
        let mut touched_history_ids = HashSet::new();
        let mut pool_output = OutboxPoolOutput::default();
        let mut negentropy_demand_changes = HashMap::new();
        let relays = HashSet::from([relay.clone()]);
        let history_ids = self
            .full_history
            .ids_with_ready_pending_neg_set_for_relay(&relay)
            .into_iter()
            .collect::<HashSet<_>>();

        for history_id in history_ids {
            if grant.is_none() {
                break;
            }
            let negentropy = &mut self.negentropy;
            self.full_history.advance_pending_neg_sets_for_sub_relays(
                history_id,
                &relays,
                |start| {
                    let Some(grant) = grant.take() else {
                        return FullHistoryNegentropyStartOutcome::Retry;
                    };
                    touched_history_ids.insert(start.history_id);
                    let relay_demand = ActiveSessionRelayDemand::single(
                        start.relay_policy.demand_priority(),
                        start.relay_policy.connection_weight(),
                    );
                    match negentropy.try_start_full_history(
                        &start.relay,
                        grant.pass,
                        || start.storage.clone(),
                        start.filter.clone(),
                        start.history_id,
                        relay_demand,
                    ) {
                        NegentropyStartResult::Started(msg) => {
                            pool_output.transport_effects.push(
                                OutboxTransportEffect::SendRelayFrame {
                                    relay: start.relay.clone(),
                                    generation: grant.generation,
                                    message: msg,
                                },
                            );
                            let demand = negentropy
                                .relay(&start.relay)
                                .and_then(|data| data.active_session_relay_demand())
                                .map(active_session_relay_transport_demand);
                            negentropy_demand_changes.insert(start.relay.clone(), demand);
                            FullHistoryNegentropyStartOutcome::Started
                        }
                        NegentropyStartResult::Rejected(pass) => {
                            returned_passes.push((start.relay.clone(), pass));
                            FullHistoryNegentropyStartOutcome::Drop
                        }
                    }
                },
            );
        }

        if let Some(unused_grant) = grant {
            returned_passes.push((relay.clone(), unused_grant.pass));
        }

        for (relay, pass) in returned_passes {
            pool_output.extend(
                self.pool
                    .return_full_history_negentropy_capacity(&relay, pass),
            );
        }

        let mut full_history = FullHistoryOutput::default();
        for history_id in touched_history_ids {
            full_history.relay_demand_changes.extend(
                self.full_history
                    .refresh_relay_transport_demand_for_sub(history_id),
            );
        }

        pool_output.extend(self.request_pending_full_history_negentropy_capacity(&relay));

        FullHistoryRuntimeOutput {
            full_history,
            negentropy_demand_changes,
            pool: pool_output,
        }
    }

    pub(in crate::relay::outbox) fn apply_full_history_workflow_deadline_due(
        &mut self,
        now: Instant,
    ) -> FullHistoryRuntimeOutput {
        let mut output =
            FullHistoryRuntimeOutput::from_full_history(self.full_history.promote_due_retries(now));

        let timed_out_subs = self.full_history.timed_out_ingestion_subs(now);
        output.full_history.relay_demand_changes.extend(
            self.full_history
                .refresh_relay_transport_demand_for_subs(timed_out_subs),
        );

        let staged = self.stage_full_history_fetches_at(Vec::new(), now);
        output.extend(staged);
        output
    }

    pub(in crate::relay::outbox) fn apply_negentropy_timeout_due(
        &mut self,
        relay_id: NormRelayUrl,
        now: Instant,
    ) -> FullHistoryRuntimeOutput {
        let output = self.apply_negentropy_relay_timeout(relay_id, now);
        FullHistoryRuntimeOutput {
            full_history: FullHistoryOutput::default(),
            negentropy_demand_changes: output.negentropy_demand_changes,
            pool: output.pool,
        }
    }

    pub(in crate::relay::outbox) fn apply_full_history_local_set_ready(
        &mut self,
        history_id: FullHistorySubId,
        request_id: u64,
        storage: negentropy::NegentropyStorageVector,
    ) -> Option<FullHistoryRuntimeOutput> {
        let applied = self
            .full_history
            .apply_full_history_local_set_ready(history_id, request_id, storage);
        applied.then(|| self.advance_pending_neg_sets_for_history(history_id))
    }

    pub(in crate::relay::outbox) fn apply_full_history_local_set_failed(
        &mut self,
        history_id: FullHistorySubId,
        request_id: u64,
    ) -> Option<FullHistoryRuntimeOutput> {
        let applied = self
            .full_history
            .apply_full_history_local_set_failed(history_id, request_id);
        applied.then(|| {
            FullHistoryRuntimeOutput::from_full_history(FullHistoryOutput {
                relay_demand_changes: self
                    .full_history
                    .refresh_relay_transport_demand_for_sub(history_id),
                ..Default::default()
            })
        })
    }

    pub(in crate::relay::outbox) fn apply_full_history_local_presence_ready(
        &mut self,
        result: FullHistoryLocalPresenceResult,
    ) -> Option<FullHistoryRuntimeOutput> {
        let stage = self
            .full_history
            .apply_full_history_local_presence_result(result)?;
        Some(self.finish_full_history_fetch_stage(stage))
    }

    pub(in crate::relay::outbox) fn apply_pending_ingestion_presence_result(
        &mut self,
        result: FullHistoryPendingIngestionPresenceResult,
    ) -> FullHistoryRuntimeOutput {
        let completed = self
            .full_history
            .apply_full_history_pending_ingestion_presence_result(result);
        self.restart_full_history_rounds(&completed)
    }

    /// Restarts the verification round of each sub whose pending ingestion
    /// completed.
    pub(in crate::relay::outbox) fn restart_full_history_rounds(
        &mut self,
        history_ids: &[FullHistorySubId],
    ) -> FullHistoryRuntimeOutput {
        let mut output = FullHistoryRuntimeOutput::empty();
        for history_id in history_ids {
            output.extend(self.restart_full_history_round(*history_id));
        }
        output
    }

    fn upsert_full_history_snapshot(
        &mut self,
        snapshot: FullHistorySnapshot,
    ) -> FullHistoryTaskOutput {
        let id = snapshot.id;
        match self.full_history.upsert(snapshot) {
            FullHistoryUpsert::Unchanged => {
                let pool_output = self.refresh_full_history_fetch_policies(id);
                let negentropy_demand_changes =
                    self.refresh_full_history_active_negentropy_policies(id);
                FullHistoryTaskOutput {
                    idle_websocket_eviction_candidates: HashSet::new(),
                    output: FullHistoryRuntimeOutput {
                        full_history: FullHistoryOutput::default(),
                        negentropy_demand_changes,
                        pool: pool_output,
                    },
                }
            }
            FullHistoryUpsert::Inserted => FullHistoryTaskOutput {
                idle_websocket_eviction_candidates: HashSet::new(),
                output: FullHistoryRuntimeOutput::from_full_history(
                    self.full_history.schedule_round(id),
                ),
            },
            FullHistoryUpsert::Changed {
                added,
                removed,
                filters_changed,
            } => {
                let mut pool_output = OutboxPoolOutput::default();
                let mut negentropy_demand_changes = HashMap::new();
                let idle_websocket_eviction_candidates = removed
                    .iter()
                    .map(|removed| removed.relay.clone())
                    .collect::<HashSet<_>>();
                pool_output.extend(self.cancel_full_history_fetches_matching(
                    id,
                    |relay, filter| {
                        removed.iter().any(|removed| {
                            &removed.relay == relay
                                && removed.filter.same_canonical_attributes(filter)
                        })
                    },
                ));
                let cancel_output = self.cancel_full_history_relay_filters(id, &removed);
                pool_output.extend(cancel_output.pool);
                negentropy_demand_changes.extend(cancel_output.negentropy_demand_changes);
                pool_output.extend(self.refresh_full_history_fetch_policies(id));
                negentropy_demand_changes
                    .extend(self.refresh_full_history_active_negentropy_policies(id));
                let mut full_history_output = cancel_output.full_history;
                full_history_output.extend(if filters_changed {
                    self.full_history.schedule_round(id)
                } else {
                    self.full_history.schedule_relay_filters(id, added)
                });
                FullHistoryTaskOutput {
                    idle_websocket_eviction_candidates,
                    output: FullHistoryRuntimeOutput {
                        full_history: full_history_output,
                        negentropy_demand_changes,
                        pool: pool_output,
                    },
                }
            }
        }
    }

    fn remove_full_history_sub(&mut self, id: FullHistorySubId) -> FullHistoryTaskOutput {
        let idle_websocket_eviction_candidates = self
            .full_history
            .relay_filters(id)
            .into_iter()
            .map(|target| target.relay)
            .collect::<HashSet<_>>();
        let mut pool_output = OutboxPoolOutput::default();
        let cancel_output = self.cancel_full_history_owner(id);
        pool_output.extend(cancel_output.pool);
        pool_output.extend(self.cancel_full_history_fetches(id));
        let relay_demand_changes = self.full_history.remove(id);
        FullHistoryTaskOutput {
            idle_websocket_eviction_candidates,
            output: FullHistoryRuntimeOutput {
                full_history: FullHistoryOutput {
                    relay_demand_changes,
                    ..Default::default()
                },
                negentropy_demand_changes: cancel_output.negentropy_demand_changes,
                pool: pool_output,
            },
        }
    }

    fn cancel_full_history_fetches(&mut self, id: FullHistorySubId) -> OutboxPoolOutput {
        let mut output = OutboxPoolOutput::default();
        for fetch_id in self.pool.subs.full_history_fetch_ids(id) {
            output.extend(self.pool.clear_fetch(fetch_id));
        }
        output
    }

    fn cancel_full_history_fetches_matching(
        &mut self,
        id: FullHistorySubId,
        mut matches: impl FnMut(&NormRelayUrl, &Filter) -> bool,
    ) -> OutboxPoolOutput {
        self.pool
            .clear_full_history_fetch_relays_matching(id, |relay, filter| matches(relay, filter))
    }

    /// Refresh stored full-history fetch routing metadata for retained
    /// relay/filter legs after a full-history target policy change.
    fn refresh_full_history_fetch_policies(&mut self, id: FullHistorySubId) -> OutboxPoolOutput {
        let Some(snapshot) = self.full_history.snapshot(id) else {
            return OutboxPoolOutput::default();
        };

        self.pool
            .refresh_full_history_fetch_policies(id, |relay, filter| {
                snapshot
                    .target_for_relay_filter(relay, filter)
                    .map(|target| target.relay_pkgs())
            })
    }

    /// Refresh active full-history negentropy demand for retained relay/filter
    /// legs after a full-history target policy change.
    fn refresh_full_history_active_negentropy_policies(
        &mut self,
        id: FullHistorySubId,
    ) -> HashMap<NormRelayUrl, Option<RelayTransportDemand>> {
        let Some(snapshot) = self.full_history.snapshot(id) else {
            return HashMap::new();
        };

        let mut demand_changes = HashMap::new();
        for target in snapshot.relay_filters() {
            self.negentropy
                .relay_mut(&target.relay)
                .refresh_active_session_relay_demand_for_owner_filter(
                    id,
                    &target.filter,
                    ActiveSessionRelayDemand::single(
                        target.demand_priority(),
                        target.relay_policy.connection_weight(),
                    ),
                );
            demand_changes.insert(
                target.relay.clone(),
                self.negentropy_transport_demand_for(&target.relay),
            );
        }
        demand_changes
    }

    /// Cancel relay-local negentropy work still owned by one tracked sub.
    fn cancel_full_history_owner(&mut self, id: FullHistorySubId) -> ServiceNegentropyOutput {
        ServiceNegentropyOutput {
            pool: self.pool.cancel_full_history_negentropy_owner(id),
            ..ServiceNegentropyOutput::empty()
        }
    }

    /// Cancel relay-local negentropy work for removed relay/filter pairs.
    fn cancel_full_history_relay_filters(
        &mut self,
        id: FullHistorySubId,
        relay_filters: &[crate::relay::FullHistoryRelayFilter],
    ) -> ServiceNegentropyOutput {
        ServiceNegentropyOutput {
            pool: self
                .pool
                .cancel_full_history_negentropy_relay_filters(id, relay_filters),
            ..ServiceNegentropyOutput::empty()
        }
    }

    fn stage_full_history_fetches(
        &mut self,
        needs: Vec<FullHistoryNeed>,
    ) -> FullHistoryRuntimeOutput {
        self.stage_full_history_fetches_at(needs, Instant::now())
    }

    fn stage_full_history_fetches_at(
        &mut self,
        needs: Vec<FullHistoryNeed>,
        now: Instant,
    ) -> FullHistoryRuntimeOutput {
        FullHistoryRuntimeOutput::from_full_history(
            self.full_history.stage_need_fetches(needs, now),
        )
    }

    fn finish_full_history_fetch_stage(
        &mut self,
        stage: LocalPresenceStage,
    ) -> FullHistoryRuntimeOutput {
        let mut output = FullHistoryRuntimeOutput::from_full_history(stage.output);
        for history_id in stage.verification_ready {
            output.extend(self.restart_full_history_round(history_id));
        }
        output
    }

    fn restart_full_history_round(
        &mut self,
        history_id: FullHistorySubId,
    ) -> FullHistoryRuntimeOutput {
        let cancel_output = self.cancel_full_history_owner(history_id);
        FullHistoryRuntimeOutput {
            pool: cancel_output.pool,
            full_history: self.full_history.restart_round(history_id),
            negentropy_demand_changes: cancel_output.negentropy_demand_changes,
        }
    }

    fn apply_negentropy_relay_timeout(
        &mut self,
        relay_id: NormRelayUrl,
        now: Instant,
    ) -> ServiceNegentropyOutput {
        if !self.relay.transport.subids_supported(&relay_id) {
            return ServiceNegentropyOutput::empty();
        }
        if self
            .negentropy
            .next_timeout_deadline(&relay_id)
            .is_none_or(|deadline| deadline > now)
        {
            return ServiceNegentropyOutput::empty();
        }

        ServiceNegentropyOutput {
            pool: self.pool.apply_negentropy_timeout(&relay_id, now),
            ..ServiceNegentropyOutput::empty()
        }
    }

    fn apply_negentropy_effects_after_release(
        &mut self,
        relay: &NormRelayUrl,
        mut effects: NegentropyRelayEffects,
        now: Instant,
    ) -> NegentropyRelease {
        let surfaced_needs = self.full_history_needs_for_relay(relay, effects.take_needs());
        let full_history =
            self.schedule_negentropy_retries_for_relay(relay, effects.take_retries(), now);
        let pool = self
            .pool
            .apply_negentropy_effects_after_release(relay, effects);
        NegentropyRelease {
            pool,
            surfaced_needs,
            full_history,
        }
    }

    fn request_pending_full_history_negentropy_capacity(
        &mut self,
        relay: &NormRelayUrl,
    ) -> OutboxPoolOutput {
        if self
            .full_history
            .ids_with_ready_pending_neg_set_for_relay(relay)
            .is_empty()
        {
            return OutboxPoolOutput::default();
        }

        self.pool
            .request_full_history_negentropy_capacity(relay)
            .unwrap_or_default()
    }

    /// Convert relay-scoped ids returned by one full-history negentropy relay.
    fn full_history_needs_for_relay(
        &self,
        relay_url: &NormRelayUrl,
        relay_needs: Vec<NegentropyNeed>,
    ) -> Vec<FullHistoryNeed> {
        let mut needs = Vec::new();
        if !self.pool.has_relay(relay_url) {
            return needs;
        }
        for need in relay_needs {
            let history_id = need.owner_history_id;
            let Some(target) =
                self.full_history
                    .target_for_relay_filter(history_id, relay_url, &need.filter)
            else {
                continue;
            };
            needs.push(FullHistoryNeed {
                history_id,
                target,
                id: need.id,
            });
        }
        needs
    }

    /// Schedule full-history retries for current relay/filter targets that
    /// failed transiently inside a relay-local negentropy session.
    fn schedule_negentropy_retries_for_relay(
        &mut self,
        relay_url: &NormRelayUrl,
        retries: Vec<NegentropyRetry>,
        now: Instant,
    ) -> FullHistoryOutput {
        let mut touched_history_ids = HashSet::new();
        if !self.pool.has_relay(relay_url) {
            return FullHistoryOutput::default();
        }

        for retry in retries {
            let history_id = retry.owner_history_id;
            let Some(target) =
                self.full_history
                    .target_for_relay_filter(history_id, relay_url, &retry.filter)
            else {
                continue;
            };
            if self
                .full_history
                .schedule_relay_filter_retry(history_id, target, now)
            {
                touched_history_ids.insert(history_id);
            }
        }

        FullHistoryOutput {
            relay_demand_changes: self
                .full_history
                .refresh_relay_transport_demand_for_subs(touched_history_ids),
            ..Default::default()
        }
    }

    /// Advance one tracked full-history sub's pending negentropy builds and
    /// start relay sessions once both storage and relay capacity are available.
    fn advance_pending_neg_sets_for_sub(
        &mut self,
        history_id: FullHistorySubId,
    ) -> ServiceNegentropyOutput {
        let mut pool_outputs = Vec::new();
        let pool = &mut self.pool;
        self.full_history
            .advance_pending_neg_sets_for_sub(history_id, |start| {
                match pool.request_full_history_negentropy_capacity(&start.relay) {
                    Ok(output) => {
                        pool_outputs.push(output);
                        FullHistoryNegentropyStartOutcome::Retry
                    }
                    Err(NegentropyCapacityError::Retry) => FullHistoryNegentropyStartOutcome::Retry,
                    Err(NegentropyCapacityError::Drop) => FullHistoryNegentropyStartOutcome::Drop,
                }
            });
        let mut output = ServiceNegentropyOutput {
            full_history: FullHistoryOutput::default(),
            pool: OutboxPoolOutput::default(),
            negentropy_demand_changes: HashMap::new(),
        };
        for pool_output in pool_outputs {
            output.pool.extend(pool_output);
        }
        output
    }

    fn advance_pending_neg_sets_for_history(
        &mut self,
        history_id: FullHistorySubId,
    ) -> FullHistoryRuntimeOutput {
        let mut output = self
            .advance_pending_neg_sets_for_sub(history_id)
            .into_full_history_runtime_output();
        output.full_history.relay_demand_changes.extend(
            self.full_history
                .refresh_relay_transport_demand_for_sub(history_id),
        );
        output
    }

    fn negentropy_transport_demand_for(
        &self,
        demand_relay: &NormRelayUrl,
    ) -> Option<RelayTransportDemand> {
        let active = self
            .negentropy
            .relay(demand_relay)
            .and_then(|data| data.active_session_relay_demand())?;
        Some(active_session_relay_transport_demand(active))
    }
}

fn active_session_relay_transport_demand(active: ActiveSessionRelayDemand) -> RelayTransportDemand {
    RelayTransportDemand::new(
        active.priority,
        RelayUrlSource::Explicit,
        active.connection_weight,
    )
}
