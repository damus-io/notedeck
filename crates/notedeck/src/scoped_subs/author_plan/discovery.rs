use enostr::{
    NormRelayUrl, OutboxIdRegistry, OutboxSubId, Pubkey, RelayDemandPriority, RelayReqStatus,
    RelayRoutingPreference, RelayUrlPkgs,
};
use hashbrown::HashSet;
use nostrdb::Filter;
use std::time::{Duration, Instant};

use super::super::ScopedSubOutboxOps;

pub(super) const RELAY_LIST_DISCOVERY_AUTHORS_PER_REQ: usize = 500;
/// Grace period for slower selected-account read relays after one discovery
/// leg has reached EOSE for the same author chunk.
pub(super) const RELAY_LIST_DISCOVERY_EOSE_GRACE: Duration = Duration::from_secs(10);
const RELAY_LIST_DISCOVERY_MAX_BACKOFF_ATTEMPTS: u32 = 4;
const RELAY_LIST_DISCOVERY_RETRY_BASE: Duration = Duration::from_secs(5);
const RELAY_LIST_DISCOVERY_RETRY_MAX: Duration = Duration::from_secs(60);

/// One tracked relay kind `10002` discovery request for a parent author chunk.
#[derive(Clone, Debug)]
pub(super) struct RelayListDiscoveryLeg {
    pub(super) id: Option<OutboxSubId>,
    pub(super) relay: NormRelayUrl,
    filter: Filter,
    pub(super) retry_attempts: u32,
    pub(super) retry_after: Option<Instant>,
}

/// Retained kind `10002` discovery for a pending author-outbox plan.
#[derive(Clone, Debug)]
pub(super) struct RelayListDiscovery {
    pub(super) chunks: Vec<RelayListDiscoveryChunk>,
}

/// One author chunk being discovered across the selected account read relays.
#[derive(Clone, Debug)]
pub(super) struct RelayListDiscoveryChunk {
    pub(super) legs: Vec<RelayListDiscoveryLeg>,
    /// Deadline after the first EOSE in this chunk; remaining legs are cleared
    /// when it elapses.
    pub(super) eose_grace_deadline: Option<Instant>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RelayListDiscoveryAdvance {
    Waiting,
    Complete,
}

impl RelayListDiscovery {
    /// Apply one relay request status fact to the matching retained discovery leg.
    pub(super) fn apply_relay_req_status(
        &mut self,
        id: OutboxSubId,
        relay: &NormRelayUrl,
        status: Option<RelayReqStatus>,
    ) -> (RelayListDiscoveryAdvance, ScopedSubOutboxOps) {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        for chunk in &mut self.chunks {
            outbox_ops.extend(chunk.apply_relay_req_status(id, relay, status));
        }

        let advance = if self.chunks.iter().all(RelayListDiscoveryChunk::is_complete) {
            RelayListDiscoveryAdvance::Complete
        } else {
            RelayListDiscoveryAdvance::Waiting
        };
        (advance, outbox_ops)
    }

    /// Apply the retained retry deadline to discovery legs whose backoff has
    /// elapsed.
    pub(super) fn apply_retry_due(
        &mut self,
        now: Instant,
    ) -> (RelayListDiscoveryAdvance, ScopedSubOutboxOps) {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        for chunk in &mut self.chunks {
            outbox_ops.extend(chunk.apply_retry_due(now));
        }

        let advance = if self.chunks.iter().all(RelayListDiscoveryChunk::is_complete) {
            RelayListDiscoveryAdvance::Complete
        } else {
            RelayListDiscoveryAdvance::Waiting
        };
        (advance, outbox_ops)
    }

    pub(super) fn unsubscribe_all(self) -> ScopedSubOutboxOps {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        for chunk in self.chunks {
            outbox_ops.extend(chunk.unsubscribe_all());
        }
        outbox_ops
    }

    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.chunks
            .iter()
            .filter_map(RelayListDiscoveryChunk::next_deadline)
            .min()
    }
}

impl RelayListDiscoveryChunk {
    fn new(
        ids: &OutboxIdRegistry,
        authors: Vec<Pubkey>,
        relays: &[NormRelayUrl],
    ) -> (Self, ScopedSubOutboxOps) {
        let filter = relay_list_discovery_filter(&authors);
        let mut outbox_ops = ScopedSubOutboxOps::default();
        let legs = relays
            .iter()
            .cloned()
            .map(|relay| {
                let (leg, ops) = RelayListDiscoveryLeg::new(ids, relay, filter.clone());
                outbox_ops.extend(ops);
                leg
            })
            .collect();
        (
            Self {
                legs,
                eose_grace_deadline: None,
            },
            outbox_ops,
        )
    }

    #[cfg(test)]
    pub(super) fn authors_for_test(&self) -> Vec<Pubkey> {
        self.legs
            .first()
            .map(|leg| crate::author_outbox::filter_author_pubkeys(&leg.filter))
            .unwrap_or_default()
    }

    fn apply_relay_req_status(
        &mut self,
        id: OutboxSubId,
        relay: &NormRelayUrl,
        status: Option<RelayReqStatus>,
    ) -> ScopedSubOutboxOps {
        if self.is_complete() {
            return ScopedSubOutboxOps::default();
        }

        let mut completed_by_eose = false;
        let mut outbox_ops = ScopedSubOutboxOps::default();
        for leg in &mut self.legs {
            if leg.matches(id, relay) {
                let was_complete = leg.is_complete();
                outbox_ops = leg.apply_relay_req_status(status);
                completed_by_eose = !was_complete
                    && matches!(status, Some(RelayReqStatus::Eose))
                    && leg.is_complete();
                break;
            }
        }

        if self.is_complete() {
            self.eose_grace_deadline = None;
            return outbox_ops;
        }

        if completed_by_eose && self.eose_grace_deadline.is_none() {
            self.eose_grace_deadline = Some(Instant::now() + RELAY_LIST_DISCOVERY_EOSE_GRACE);
        }

        outbox_ops
    }

    fn apply_retry_due(&mut self, now: Instant) -> ScopedSubOutboxOps {
        if self.is_complete() {
            return ScopedSubOutboxOps::default();
        }

        if self
            .eose_grace_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            return self.unsubscribe_all_mut();
        }

        let mut outbox_ops = ScopedSubOutboxOps::default();
        for leg in &mut self.legs {
            outbox_ops.extend(leg.apply_retry_due(now));
        }
        if self.is_complete() {
            self.eose_grace_deadline = None;
        }
        outbox_ops
    }

    fn unsubscribe_all(mut self) -> ScopedSubOutboxOps {
        self.unsubscribe_all_mut()
    }

    fn unsubscribe_all_mut(&mut self) -> ScopedSubOutboxOps {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        for leg in &mut self.legs {
            let Some(id) = leg.id.take() else {
                continue;
            };
            outbox_ops.clear_fetch(id);
        }
        self.eose_grace_deadline = None;
        outbox_ops
    }

    fn next_deadline(&self) -> Option<Instant> {
        if self.is_complete() {
            return None;
        }

        let leg_deadline = self
            .legs
            .iter()
            .filter_map(RelayListDiscoveryLeg::next_deadline)
            .min();
        [self.eose_grace_deadline, leg_deadline]
            .into_iter()
            .flatten()
            .min()
    }

    fn is_complete(&self) -> bool {
        !self.legs.is_empty() && self.legs.iter().all(RelayListDiscoveryLeg::is_complete)
    }
}

impl RelayListDiscoveryLeg {
    fn new(
        ids: &OutboxIdRegistry,
        relay: NormRelayUrl,
        filter: Filter,
    ) -> (Self, ScopedSubOutboxOps) {
        let (id, outbox_ops) =
            subscribe_relay_list_discovery_leg(ids, relay.clone(), filter.clone());
        (
            Self {
                id: Some(id),
                relay,
                filter,
                retry_attempts: 0,
                retry_after: None,
            },
            outbox_ops,
        )
    }

    fn apply_relay_req_status(&mut self, status: Option<RelayReqStatus>) -> ScopedSubOutboxOps {
        match status {
            Some(RelayReqStatus::Eose) => {
                self.id = None;
                self.retry_after = None;
                ScopedSubOutboxOps::default()
            }
            Some(RelayReqStatus::Closed) => self.record_failed_attempt(),
            Some(RelayReqStatus::InitialQuery) | None => ScopedSubOutboxOps::default(),
        }
    }

    fn apply_retry_due(&mut self, now: Instant) -> ScopedSubOutboxOps {
        if self.is_complete() {
            return ScopedSubOutboxOps::default();
        }
        let Some(retry_after) = self.retry_after else {
            return ScopedSubOutboxOps::default();
        };
        if now < retry_after {
            return ScopedSubOutboxOps::default();
        }
        let Some(id) = self.id else {
            return ScopedSubOutboxOps::default();
        };

        let outbox_ops =
            stage_relay_list_discovery_leg(id, self.relay.clone(), self.filter.clone());
        self.retry_after = None;
        outbox_ops
    }

    fn matches(&self, id: OutboxSubId, relay: &NormRelayUrl) -> bool {
        self.id == Some(id) && &self.relay == relay
    }

    fn record_failed_attempt(&mut self) -> ScopedSubOutboxOps {
        if self.retry_attempts >= RELAY_LIST_DISCOVERY_MAX_BACKOFF_ATTEMPTS {
            let Some(id) = self.id.take() else {
                return ScopedSubOutboxOps::default();
            };
            self.retry_after = None;
            let mut outbox_ops = ScopedSubOutboxOps::default();
            outbox_ops.clear_fetch(id);
            return outbox_ops;
        }

        self.retry_attempts = self.retry_attempts.saturating_add(1);
        self.retry_after = Some(Instant::now() + self.retry_delay());
        ScopedSubOutboxOps::default()
    }

    fn retry_delay(&self) -> Duration {
        let shift = self.retry_attempts.saturating_sub(1).min(4);
        let factor = 1u32 << shift;
        RELAY_LIST_DISCOVERY_RETRY_BASE
            .saturating_mul(factor)
            .min(RELAY_LIST_DISCOVERY_RETRY_MAX)
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.retry_after
    }

    fn is_complete(&self) -> bool {
        self.id.is_none()
    }
}

pub(super) fn start_relay_list_discovery(
    ids: &OutboxIdRegistry,
    authors: HashSet<Pubkey>,
    relays: HashSet<NormRelayUrl>,
) -> (RelayListDiscovery, ScopedSubOutboxOps) {
    let author_chunks = relay_list_discovery_author_chunks(&authors);
    let mut relays = relays.into_iter().collect::<Vec<_>>();
    relays.sort();
    let mut outbox_ops = ScopedSubOutboxOps::default();
    let chunks = author_chunks
        .into_iter()
        .map(|authors| {
            let (chunk, ops) = RelayListDiscoveryChunk::new(ids, authors, &relays);
            outbox_ops.extend(ops);
            chunk
        })
        .collect();
    (RelayListDiscovery { chunks }, outbox_ops)
}

fn subscribe_relay_list_discovery_leg(
    ids: &OutboxIdRegistry,
    relay: NormRelayUrl,
    filter: Filter,
) -> (OutboxSubId, ScopedSubOutboxOps) {
    let id = ids.next_sub_id();
    let outbox_ops = stage_relay_list_discovery_leg(id, relay, filter);
    (id, outbox_ops)
}

fn stage_relay_list_discovery_leg(
    id: OutboxSubId,
    relay: NormRelayUrl,
    filter: Filter,
) -> ScopedSubOutboxOps {
    let mut outbox_ops = ScopedSubOutboxOps::default();
    outbox_ops.start_fetch(
        id,
        vec![filter],
        RelayUrlPkgs::new(
            HashSet::from([relay]),
            enostr::RelayUrlPolicy::explicit(
                RelayDemandPriority::Important,
                RelayRoutingPreference::PreferDedicated,
            ),
        ),
    );
    outbox_ops
}

fn relay_list_discovery_author_chunks(authors: &HashSet<Pubkey>) -> Vec<Vec<Pubkey>> {
    let mut authors = authors.iter().copied().collect::<Vec<_>>();
    authors.sort_unstable();
    authors
        .chunks(RELAY_LIST_DISCOVERY_AUTHORS_PER_REQ)
        .map(<[Pubkey]>::to_vec)
        .collect()
}

fn relay_list_discovery_filter(authors: &[Pubkey]) -> Filter {
    Filter::new()
        .authors(authors.iter().map(Pubkey::bytes))
        .kinds([10002])
        .build()
}
