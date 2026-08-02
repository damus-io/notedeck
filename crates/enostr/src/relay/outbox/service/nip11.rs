use std::{
    collections::hash_map::DefaultHasher,
    future::Future,
    hash::{Hash, Hasher},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use hashbrown::{HashMap, HashSet};
use tokio::sync::mpsc;

use crate::relay::{Nip11LimitationsRaw, NormRelayUrl, RelayConnectionPriority};

use super::next_backoff_duration_from_base;

pub(super) const NIP11_FETCH_CONCURRENCY: usize = 8;
pub(super) const NIP11_REFRESH_AFTER_SUCCESS: Duration = Duration::from_secs(60 * 60);
pub(super) const NIP11_FAILURE_BACKOFF_BASE: Duration = Duration::from_secs(5);
pub(super) const MAX_NIP11_FAILURE_BACKOFF: Duration = Duration::from_secs(30 * 60);
pub(super) const NIP11_APPLY_DEFERRED_BACKOFF_BASE: Duration = Duration::from_millis(250);
pub(super) const MAX_NIP11_APPLY_DEFERRED_BACKOFF: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Nip11FetchRequest {
    pub relay: NormRelayUrl,
    pub attempt: u32,
}

pub trait Nip11Capability: Send + Sync + 'static {
    type Output;
    type Future: Future<Output = Self::Output> + Send + 'static;

    fn fetch_nip11(&self, request: Nip11FetchRequest) -> Self::Future;
}

#[derive(Debug)]
pub(super) struct Nip11ReadinessInput {
    pub(super) now: SystemTime,
    pub(super) interests: Vec<Nip11InterestRead>,
}

#[derive(Debug)]
pub(super) struct Nip11InterestRead {
    pub(super) relay: NormRelayUrl,
    pub(super) rank: Nip11InterestRank,
    pub(super) state: Nip11InterestState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Nip11InterestRank {
    pub(super) priority: Option<RelayConnectionPriority>,
    pub(super) source_rank: u8,
    pub(super) connection_weight: u32,
    pub(super) health_rank: (bool, u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Nip11InterestState {
    Active,
    Suspended(Nip11InterestResume),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Nip11InterestResume {
    At(SystemTime),
    OnRelayInput,
}

#[derive(Debug)]
pub(super) enum Nip11ServiceOutput {
    Stale,
    FetchFailed,
    Limits {
        request: Nip11FetchRequest,
        raw: Box<Nip11LimitationsRaw>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Nip11ApplyAck {
    Consumed,
    Deferred,
}

struct Nip11FetchResult {
    request: Nip11FetchRequest,
    result: Result<Nip11LimitationsRaw, String>,
}

/// Owns NIP-11 metadata fetch state and host capability execution.
pub(super) struct Nip11Service<N> {
    driver: Nip11Driver,
    capability: N,
    results_tx: mpsc::UnboundedSender<Nip11FetchResult>,
    results_rx: mpsc::UnboundedReceiver<Nip11FetchResult>,
}

impl<N> Nip11Service<N>
where
    N: Nip11Capability<Output = Result<Nip11LimitationsRaw, String>>,
{
    pub(super) fn new(capability: N) -> Self {
        let (results_tx, results_rx) = mpsc::unbounded_channel();
        Self {
            driver: Nip11Driver::default(),
            capability,
            results_tx,
            results_rx,
        }
    }

    #[cfg(test)]
    pub(super) fn next_deadline(&self) -> Option<SystemTime> {
        self.driver.next_deadline()
    }

    pub(super) fn next_readiness_deadline(
        &mut self,
        input: Nip11ReadinessInput,
    ) -> Option<SystemTime> {
        self.driver.retain_interests(&input.interests, input.now);
        let has_capacity = self.driver.available_fetch_capacity() > 0;
        let has_ready_interest = !self
            .driver
            .ready_interests(input.interests, input.now)
            .is_empty();
        let next_deadline = self.driver.next_deadline();
        if has_capacity && has_ready_interest {
            Some(input.now)
        } else {
            next_deadline
        }
    }

    pub(super) fn apply_readiness(&mut self, input: Nip11ReadinessInput) {
        self.driver.retain_interests(&input.interests, input.now);
        let capacity = self.driver.available_fetch_capacity();
        if capacity == 0 {
            return;
        }

        let mut ready = self.driver.ready_interests(input.interests, input.now);
        ready.sort_by(cmp_nip11_interest);
        for interest in ready.into_iter().take(capacity) {
            let Some(request) = self.driver.start_fetch(interest.relay, input.now) else {
                continue;
            };
            self.start_fetch(request);
        }
    }

    pub(super) async fn recv(&mut self) -> Option<Nip11ServiceOutput> {
        let result = self.results_rx.recv().await?;
        Some(self.apply_fetch_result(result, SystemTime::now()))
    }

    pub(super) fn apply_limits_ack(
        &mut self,
        request: Nip11FetchRequest,
        ack: Nip11ApplyAck,
        now: SystemTime,
    ) {
        if !self.driver.is_current_result(&request) {
            return;
        }

        match ack {
            Nip11ApplyAck::Consumed => self.driver.mark_success(&request.relay, now),
            Nip11ApplyAck::Deferred => self.driver.mark_apply_deferred(&request.relay, now),
        }
    }

    fn apply_fetch_result(
        &mut self,
        result: Nip11FetchResult,
        now: SystemTime,
    ) -> Nip11ServiceOutput {
        if !self.driver.is_current_result(&result.request) {
            return Nip11ServiceOutput::Stale;
        }

        match result.result {
            Ok(raw) => Nip11ServiceOutput::Limits {
                request: result.request,
                raw: Box::new(raw),
            },
            Err(error) => {
                self.driver.mark_failure(&result.request.relay, error, now);
                Nip11ServiceOutput::FetchFailed
            }
        }
    }

    fn start_fetch(&mut self, request: Nip11FetchRequest) {
        let future = self.capability.fetch_nip11(request.clone());
        self.start_future(future, move |result| Nip11FetchResult { request, result });
    }

    fn start_future<Fut, Map>(&mut self, future: Fut, map: Map)
    where
        Fut: Future + Send + 'static,
        Fut::Output: Send + 'static,
        Map: FnOnce(Fut::Output) -> Nip11FetchResult + Send + 'static,
    {
        let tx = self.results_tx.clone();
        tokio::spawn(async move {
            let result = map(future.await);
            let _ = tx.send(result);
        });
    }
}

#[derive(Default)]
pub(super) struct Nip11Driver {
    pub(super) relays: HashMap<NormRelayUrl, Nip11RelayFetchState>,
}

#[derive(Default)]
pub(super) struct Nip11RelayFetchState {
    pub(super) in_flight_attempt: Option<u32>,
    pub(super) next_fetch_at: Option<SystemTime>,
    pub(super) attempt: u32,
    pub(super) suspended: Option<Nip11InterestResume>,
}

impl Nip11Driver {
    pub(super) fn next_deadline(&self) -> Option<SystemTime> {
        self.relays
            .values()
            .filter(|state| state.in_flight_attempt.is_none())
            .filter_map(|state| state.next_deadline())
            .min()
    }

    pub(super) fn available_fetch_capacity(&self) -> usize {
        NIP11_FETCH_CONCURRENCY.saturating_sub(self.in_flight_count())
    }

    fn in_flight_count(&self) -> usize {
        self.relays
            .values()
            .filter(|state| state.in_flight_attempt.is_some())
            .count()
    }

    pub(super) fn retain_interests(&mut self, interests: &[Nip11InterestRead], now: SystemTime) {
        let relays = interests
            .iter()
            .map(|interest| interest.relay.clone())
            .collect::<HashSet<_>>();
        self.relays.retain(|relay, state| {
            relays.contains(relay)
                || state.in_flight_attempt.is_some()
                || state.next_fetch_at.is_some_and(|deadline| deadline > now)
        });
    }

    pub(super) fn ready_interests(
        &mut self,
        interests: impl IntoIterator<Item = Nip11InterestRead>,
        now: SystemTime,
    ) -> Vec<Nip11InterestRead> {
        interests
            .into_iter()
            .filter_map(|interest| {
                let state = self.relays.entry(interest.relay.clone()).or_default();
                state.apply_interest(interest.state);
                state.ready_to_fetch(now).then_some(interest)
            })
            .collect()
    }

    pub(super) fn start_fetch(
        &mut self,
        relay: NormRelayUrl,
        now: SystemTime,
    ) -> Option<Nip11FetchRequest> {
        let state = self.relays.get_mut(&relay)?;
        if !state.ready_to_fetch(now) {
            return None;
        }

        let attempt = state.next_attempt();
        state.mark_dispatched(attempt);
        Some(Nip11FetchRequest { relay, attempt })
    }

    pub(super) fn is_current_result(&self, request: &Nip11FetchRequest) -> bool {
        self.relays
            .get(&request.relay)
            .is_some_and(|state| state.in_flight_attempt == Some(request.attempt))
    }

    pub(super) fn mark_success(&mut self, relay: &NormRelayUrl, now: SystemTime) {
        if let Some(state) = self.relays.get_mut(relay) {
            state.in_flight_attempt = None;
            state.attempt = 0;
            state.next_fetch_at = now.checked_add(NIP11_REFRESH_AFTER_SUCCESS);
            state.suspended = None;
        }
    }

    pub(super) fn mark_failure(&mut self, relay: &NormRelayUrl, error: String, now: SystemTime) {
        if let Some(state) = self.relays.get_mut(relay) {
            let retry_after = nip11_failure_retry_after(relay, state.attempt);
            tracing::debug!("nip11: {relay} fetch failed: {error} (retry in {retry_after:?})");
            state.in_flight_attempt = None;
            state.next_fetch_at = now.checked_add(retry_after);
            state.suspended = None;
        }
    }

    pub(super) fn mark_apply_deferred(&mut self, relay: &NormRelayUrl, now: SystemTime) {
        if let Some(state) = self.relays.get_mut(relay) {
            let retry_after = nip11_apply_deferred_retry_after(relay, state.attempt);
            tracing::debug!("nip11: {relay} limits apply deferred (retry in {retry_after:?})");
            state.in_flight_attempt = None;
            state.next_fetch_at = now.checked_add(retry_after);
            state.suspended = None;
        }
    }
}

impl Nip11RelayFetchState {
    pub(super) fn next_deadline(&self) -> Option<SystemTime> {
        if self.in_flight_attempt.is_some() {
            return None;
        }

        match self.suspended {
            Some(Nip11InterestResume::At(deadline)) => Some(deadline),
            Some(Nip11InterestResume::OnRelayInput) => None,
            None => self.next_fetch_at,
        }
    }

    pub(super) fn ready_to_fetch(&self, now: SystemTime) -> bool {
        if self.in_flight_attempt.is_some() {
            return false;
        }
        if self.suspended.is_some() {
            return false;
        }
        self.next_fetch_at.is_none_or(|at| now >= at)
    }

    pub(super) fn apply_interest(&mut self, state: Nip11InterestState) {
        self.suspended = match state {
            Nip11InterestState::Active => None,
            Nip11InterestState::Suspended(resume) => Some(resume),
        };
    }

    pub(super) fn next_attempt(&self) -> u32 {
        self.attempt.saturating_add(1)
    }

    pub(super) fn mark_dispatched(&mut self, attempt: u32) {
        self.attempt = attempt;
        self.in_flight_attempt = Some(attempt);
    }
}

pub(super) fn nip11_failure_retry_after(relay: &NormRelayUrl, attempt: u32) -> Duration {
    let seed = nip11_jitter_seed(relay, attempt);
    next_backoff_duration_from_base(
        attempt,
        NIP11_FAILURE_BACKOFF_BASE,
        seed,
        MAX_NIP11_FAILURE_BACKOFF,
    )
}

fn nip11_apply_deferred_retry_after(relay: &NormRelayUrl, attempt: u32) -> Duration {
    let seed = nip11_jitter_seed(relay, attempt);
    next_backoff_duration_from_base(
        attempt,
        NIP11_APPLY_DEFERRED_BACKOFF_BASE,
        seed,
        MAX_NIP11_APPLY_DEFERRED_BACKOFF,
    )
}

fn nip11_jitter_seed(relay: &NormRelayUrl, attempt: u32) -> u64 {
    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mut hasher = DefaultHasher::new();
    relay.hash(&mut hasher);
    attempt.hash(&mut hasher);
    now_nanos.hash(&mut hasher);
    hasher.finish()
}

fn cmp_nip11_interest(left: &Nip11InterestRead, right: &Nip11InterestRead) -> std::cmp::Ordering {
    let left_rank = left.rank;
    let right_rank = right.rank;
    let demand_class = match (left_rank.priority, right_rank.priority) {
        (Some(left), Some(right)) => right.strongest_demand.cmp(&left.strongest_demand),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    };
    demand_class
        .then_with(|| right_rank.source_rank.cmp(&left_rank.source_rank))
        .then_with(|| {
            right_rank
                .connection_weight
                .cmp(&left_rank.connection_weight)
        })
        .then_with(|| {
            let left_count = left_rank
                .priority
                .map(|priority| priority.request_count)
                .unwrap_or_default();
            let right_count = right_rank
                .priority
                .map(|priority| priority.request_count)
                .unwrap_or_default();
            right_count.cmp(&left_count)
        })
        .then_with(|| left_rank.health_rank.cmp(&right_rank.health_rank))
        .then_with(|| left.relay.cmp(&right.relay))
}
