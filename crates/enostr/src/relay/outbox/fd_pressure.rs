use crate::relay::{RelayConnectionPriority, RelayDemandPriority};
use crate::WebSocketError;

#[cfg(unix)]
use std::fs;
use std::time::{Duration, Instant};

const LOW_PRIORITY_STOP_PERCENT: usize = 75;
const LOW_PRIORITY_RESUME_PERCENT: usize = 65;
const FALLBACK_LOW_PRIORITY_STOP_WEBSOCKETS: usize = 32;
const FALLBACK_LOW_PRIORITY_RESUME_WEBSOCKETS: usize = 24;
const FD_SNAPSHOT_CACHE_TTL: Duration = Duration::from_secs(1);
const HARD_FAILURE_COOLDOWN: Duration = Duration::from_secs(30);

/// Snapshot of the current process file-descriptor usage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProcessFdSnapshot {
    open_fds: usize,
    soft_limit: usize,
}

impl ProcessFdSnapshot {
    /// Construct one explicit snapshot value.
    pub(crate) fn new(open_fds: usize, soft_limit: usize) -> Self {
        Self {
            open_fds,
            soft_limit,
        }
    }

    fn low_priority_stop_watermark(self) -> usize {
        self.soft_limit.saturating_mul(LOW_PRIORITY_STOP_PERCENT) / 100
    }

    fn low_priority_resume_watermark(self) -> usize {
        self.soft_limit.saturating_mul(LOW_PRIORITY_RESUME_PERCENT) / 100
    }
}

/// Source of the current relay-connection pressure signal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PressureSource {
    /// Pressure derived from a process-level file-descriptor snapshot.
    ProcessFdSnapshot(ProcessFdSnapshot),
    /// Pressure derived from a conservative local websocket-count cap when fd
    /// telemetry is unavailable.
    Fallback { open_websockets: usize },
}

impl PressureSource {
    fn stop_watermark(self) -> usize {
        match self {
            PressureSource::ProcessFdSnapshot(snapshot) => snapshot.low_priority_stop_watermark(),
            PressureSource::Fallback { .. } => FALLBACK_LOW_PRIORITY_STOP_WEBSOCKETS,
        }
    }

    fn resume_watermark(self) -> usize {
        match self {
            PressureSource::ProcessFdSnapshot(snapshot) => snapshot.low_priority_resume_watermark(),
            PressureSource::Fallback { .. } => FALLBACK_LOW_PRIORITY_RESUME_WEBSOCKETS,
        }
    }

    fn projected_usage(self, websocket_delta: isize) -> usize {
        let base_usage = match self {
            PressureSource::ProcessFdSnapshot(snapshot) => snapshot.open_fds,
            PressureSource::Fallback { open_websockets } => open_websockets,
        };

        if websocket_delta >= 0 {
            return base_usage.saturating_add(websocket_delta as usize);
        }

        base_usage.saturating_sub(websocket_delta.unsigned_abs())
    }
}

/// Current websocket-open pressure mode for one outbox batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PressureState {
    /// Relay websocket opens are operating under normal conditions.
    Clear(PressureSource),
    /// Soft pressure is active and low-value relay demand should be throttled.
    SoftConstrained(PressureSource),
    /// A recent hard fd-exhaustion signal was observed and direct opens should
    /// not proceed without first freeing lower-value websocket capacity.
    HardFailureConstrained(PressureSource),
}

/// Desired websocket-open importance for one relay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RelaySocketDemand {
    /// Admit according to the relay's derived connection priority.
    Prioritized(RelayConnectionPriority),
}

impl RelaySocketDemand {
    pub(crate) fn eviction_priority(self) -> RelayConnectionPriority {
        match self {
            Self::Prioritized(priority) => priority,
        }
    }
}

/// One relay-open decision produced from the current pressure state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RelayOpenDecision {
    /// The websocket open may proceed directly.
    Open,
    /// The open may proceed, but the pool should evict a lower-value relay
    /// first if one exists.
    TryEvictThenOpen,
    /// The open may only proceed after evicting a lower-value relay.
    RequireEviction,
    /// The relay demand should remain declared, but no websocket open should be
    /// attempted right now.
    Defer,
}

/// Admission-policy inputs that decide whether a deferred relay should be rechecked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RelayAdmissionPolicy {
    pressure_state: PressureState,
    projected_websocket_count: usize,
    max_websocket_connections: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CachedFdSnapshot {
    snapshot: ProcessFdSnapshot,
    sampled_open_websockets: usize,
    sampled_at: Instant,
}

/// Tracks websocket-open pressure state across outbox batches.
#[derive(Default)]
pub(crate) struct FdPressureGate {
    measured_low_priority_throttled: bool,
    fallback_low_priority_throttled: bool,
    hard_failure_until: Option<Instant>,
    cached_snapshot: Option<CachedFdSnapshot>,
}

impl FdPressureGate {
    pub(crate) fn start_batch_at(
        &mut self,
        open_websockets: usize,
        max_websocket_connections: Option<usize>,
        now: Instant,
    ) -> RelayOpenContext {
        self.start_batch_at_with_snapshot_reader(
            open_websockets,
            max_websocket_connections,
            now,
            current_process_fd_snapshot,
        )
    }

    pub(crate) fn read_batch_at(
        &self,
        open_websockets: usize,
        max_websocket_connections: Option<usize>,
        now: Instant,
    ) -> RelayOpenContext {
        let (source, fd_projection_delta, low_priority_throttled) = if let Some(cached) =
            self.cached_snapshot.filter(|cached| {
                now.saturating_duration_since(cached.sampled_at) < FD_SNAPSHOT_CACHE_TTL
            }) {
            (
                PressureSource::ProcessFdSnapshot(cached.snapshot),
                websocket_count_delta(open_websockets, cached.sampled_open_websockets),
                self.measured_low_priority_throttled,
            )
        } else {
            (
                PressureSource::Fallback { open_websockets },
                0,
                self.fallback_low_priority_throttled,
            )
        };

        RelayOpenContext {
            source,
            open_websockets,
            max_websocket_connections,
            low_priority_throttled,
            hard_failure_active: self
                .hard_failure_until
                .is_some_and(|deadline| now < deadline),
            fd_projection_delta,
            websocket_delta: 0,
        }
    }

    fn start_batch_at_with_snapshot_reader(
        &mut self,
        open_websockets: usize,
        max_websocket_connections: Option<usize>,
        now: Instant,
        read_snapshot: impl FnMut() -> Option<ProcessFdSnapshot>,
    ) -> RelayOpenContext {
        let (source, fd_projection_delta) = self
            .current_snapshot_at(now, open_websockets, read_snapshot)
            .map(|(snapshot, fd_projection_delta)| {
                (
                    PressureSource::ProcessFdSnapshot(snapshot),
                    fd_projection_delta,
                )
            })
            .unwrap_or((PressureSource::Fallback { open_websockets }, 0));
        self.start_batch_from_source(
            open_websockets,
            max_websocket_connections,
            source,
            fd_projection_delta,
            now,
        )
    }

    fn start_batch_from_source(
        &mut self,
        open_websockets: usize,
        max_websocket_connections: Option<usize>,
        source: PressureSource,
        fd_projection_delta: isize,
        now: Instant,
    ) -> RelayOpenContext {
        let low_priority_throttled = self.refresh_throttle_state(source, fd_projection_delta);

        RelayOpenContext {
            // Snapshot one pressure source for the whole relay-service pass, then
            // seed process-fd projection with websocket changes already reflected
            // in the pool since that snapshot. The batch delta below remains only
            // the opens/evictions performed in this pass.
            source,
            open_websockets,
            max_websocket_connections,
            low_priority_throttled,
            hard_failure_active: self.hard_failure_active_at(now),
            fd_projection_delta,
            websocket_delta: 0,
        }
    }

    pub(crate) fn enter_hard_failure_from_websocket_error(
        &mut self,
        error: &WebSocketError,
    ) -> bool {
        self.enter_hard_failure_from_websocket_error_at(error, Instant::now())
    }

    pub(crate) fn enter_hard_failure_from_websocket_error_at(
        &mut self,
        error: &WebSocketError,
        now: Instant,
    ) -> bool {
        let Some(raw_os_error) = error.raw_os_error() else {
            return false;
        };

        if raw_os_error == libc::EMFILE || raw_os_error == libc::ENFILE {
            self.hard_failure_until = Some(now + HARD_FAILURE_COOLDOWN);
            return true;
        }

        false
    }

    /// Returns the next instant when the admission policy can change without a
    /// relay open, close, or caller configuration change.
    pub(crate) fn next_policy_refresh_deadline(&self, now: Instant) -> Option<Instant> {
        [
            self.cached_snapshot
                .map(|cached| cached.sampled_at + FD_SNAPSHOT_CACHE_TTL),
            self.hard_failure_until,
        ]
        .into_iter()
        .flatten()
        .filter(|deadline| *deadline > now)
        .min()
    }

    pub(crate) fn clear_hard_failure_on_open_success(&mut self) {
        self.hard_failure_until = None;
    }

    fn refresh_throttle_state(
        &mut self,
        source: PressureSource,
        fd_projection_delta: isize,
    ) -> bool {
        match source {
            PressureSource::ProcessFdSnapshot(_) => Self::refresh_soft_constraint(
                &mut self.measured_low_priority_throttled,
                source.projected_usage(fd_projection_delta),
                source.stop_watermark(),
                source.resume_watermark(),
            ),
            PressureSource::Fallback { .. } => Self::refresh_soft_constraint(
                &mut self.fallback_low_priority_throttled,
                source.projected_usage(fd_projection_delta),
                source.stop_watermark(),
                source.resume_watermark(),
            ),
        }
    }

    fn refresh_soft_constraint(
        throttled: &mut bool,
        usage: usize,
        stop_watermark: usize,
        resume_watermark: usize,
    ) -> bool {
        if *throttled {
            *throttled = usage >= resume_watermark;
            return *throttled;
        }

        *throttled = usage >= stop_watermark;
        *throttled
    }

    fn current_snapshot_at(
        &mut self,
        now: Instant,
        open_websockets: usize,
        mut read_snapshot: impl FnMut() -> Option<ProcessFdSnapshot>,
    ) -> Option<(ProcessFdSnapshot, isize)> {
        if let Some(cached) = self.cached_snapshot {
            if now.saturating_duration_since(cached.sampled_at) < FD_SNAPSHOT_CACHE_TTL {
                return Some((
                    cached.snapshot,
                    websocket_count_delta(open_websockets, cached.sampled_open_websockets),
                ));
            }
        }

        let snapshot = read_snapshot()?;
        self.cached_snapshot = Some(CachedFdSnapshot {
            snapshot,
            sampled_open_websockets: open_websockets,
            sampled_at: now,
        });
        Some((snapshot, 0))
    }

    fn hard_failure_active_at(&mut self, now: Instant) -> bool {
        let Some(until) = self.hard_failure_until else {
            return false;
        };

        if now < until {
            return true;
        }

        self.hard_failure_until = None;
        false
    }
}

/// Admission state for websocket opens attempted within one outbox batch.
pub(crate) struct RelayOpenContext {
    source: PressureSource,
    open_websockets: usize,
    max_websocket_connections: Option<usize>,
    low_priority_throttled: bool,
    hard_failure_active: bool,
    fd_projection_delta: isize,
    websocket_delta: isize,
}

impl RelayOpenContext {
    /// Return the current admission policy state for deferral comparisons.
    pub(crate) fn policy(&self) -> RelayAdmissionPolicy {
        RelayAdmissionPolicy {
            pressure_state: self.pressure_state(),
            projected_websocket_count: self.projected_websocket_count(0),
            max_websocket_connections: self.max_websocket_connections,
        }
    }

    /// Returns the current pressure state for this batch after applying any
    /// in-batch projected websocket opens or evictions.
    pub(crate) fn pressure_state(&self) -> PressureState {
        if self.hard_failure_active {
            return PressureState::HardFailureConstrained(self.source);
        }

        if self.low_priority_throttled {
            return PressureState::SoftConstrained(self.source);
        }

        PressureState::Clear(self.source)
    }

    /// Returns the relay-open decision for one demand item under the current
    /// batch pressure state.
    pub(crate) fn decide(&self, demand: RelaySocketDemand) -> RelayOpenDecision {
        if self.websocket_limit_would_be_exceeded_by_open() {
            return RelayOpenDecision::RequireEviction;
        }

        let pressure_state = self.pressure_state();
        if matches!(pressure_state, PressureState::HardFailureConstrained(_)) {
            return RelayOpenDecision::RequireEviction;
        }

        let priority = demand.eviction_priority();

        match pressure_state {
            PressureState::Clear(source) => {
                // Important/Critical demand is allowed to keep growing until a
                // later pressure state says otherwise. Lower-value demand is
                // cut off at the projected stop watermark before it opens.
                if priority.strongest_demand >= RelayDemandPriority::Important {
                    return RelayOpenDecision::Open;
                }

                if self.projected_fd_usage(1) >= source.stop_watermark() {
                    return RelayOpenDecision::Defer;
                }

                RelayOpenDecision::Open
            }
            PressureState::SoftConstrained(_) => {
                // Under soft pressure we still admit Important/Critical demand,
                // but only after opportunistically trying to free a lower-value
                // relay first. Lower-value demand is simply deferred.
                if priority.strongest_demand >= RelayDemandPriority::Important {
                    return RelayOpenDecision::TryEvictThenOpen;
                }

                RelayOpenDecision::Defer
            }
            PressureState::HardFailureConstrained(_) => RelayOpenDecision::RequireEviction,
        }
    }

    pub(crate) fn low_value_open_allowed_without_eviction(
        &self,
        priority: RelayConnectionPriority,
    ) -> bool {
        matches!(
            self.decide(RelaySocketDemand::Prioritized(priority)),
            RelayOpenDecision::Open
        ) && self.websocket_limit_allows_open_after_evictions(0)
    }

    /// Records one successful websocket-open attempt in the current batch.
    pub(crate) fn record_socket_open(&mut self) {
        self.websocket_delta = self.websocket_delta.saturating_add(1);
        self.refresh_projected_constraint();
    }

    /// Records one websocket eviction in the current batch.
    pub(crate) fn record_socket_eviction(&mut self) {
        self.websocket_delta = self.websocket_delta.saturating_sub(1);
        self.refresh_projected_constraint();
    }

    /// Returns whether the configured websocket cap is already overfull.
    pub(crate) fn should_shed_for_websocket_limit(&self) -> bool {
        self.max_websocket_connections
            .is_some_and(|limit| self.projected_websocket_count(0) > limit)
    }

    /// Returns whether one new websocket can open after evicting `evictions`
    /// existing pressure-counted websockets in this admission context.
    pub(crate) fn websocket_limit_allows_open_after_evictions(&self, evictions: usize) -> bool {
        let evictions = isize::try_from(evictions).unwrap_or(isize::MAX);
        let projected_open_delta = 1isize.saturating_sub(evictions);
        self.max_websocket_connections
            .is_none_or(|limit| self.projected_websocket_count(projected_open_delta) <= limit)
    }

    fn refresh_projected_constraint(&mut self) {
        let usage = self.projected_fd_usage(0);
        if self.low_priority_throttled {
            self.low_priority_throttled = usage >= self.source.resume_watermark();
            return;
        }

        self.low_priority_throttled = usage >= self.source.stop_watermark();
    }

    fn websocket_limit_would_be_exceeded_by_open(&self) -> bool {
        self.max_websocket_connections
            .is_some_and(|limit| self.projected_websocket_count(1) > limit)
    }

    fn projected_websocket_count(&self, new_delta: isize) -> usize {
        let delta = self.websocket_delta.saturating_add(new_delta);
        if delta >= 0 {
            return self.open_websockets.saturating_add(delta as usize);
        }

        self.open_websockets.saturating_sub(delta.unsigned_abs())
    }

    fn projected_fd_usage(&self, new_delta: isize) -> usize {
        self.source.projected_usage(
            self.fd_projection_delta
                .saturating_add(self.websocket_delta)
                .saturating_add(new_delta),
        )
    }
}

fn websocket_count_delta(current: usize, sampled: usize) -> isize {
    if current >= sampled {
        return usize_to_isize_saturating(current - sampled);
    }

    -usize_to_isize_saturating(sampled - current)
}

fn usize_to_isize_saturating(value: usize) -> isize {
    isize::try_from(value).unwrap_or(isize::MAX)
}

#[cfg(not(unix))]
fn current_process_fd_snapshot() -> Option<ProcessFdSnapshot> {
    None
}

#[cfg(unix)]
fn current_process_fd_snapshot() -> Option<ProcessFdSnapshot> {
    let soft_limit = current_process_soft_fd_limit()?;
    let open_fds = current_process_open_fd_count()?;
    Some(ProcessFdSnapshot::new(open_fds, soft_limit))
}

#[cfg(unix)]
fn current_process_soft_fd_limit() -> Option<usize> {
    let mut limits = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    // SAFETY: `limits.as_mut_ptr()` points to writable storage for one
    // `libc::rlimit`, and `getrlimit` initializes that storage when it
    // returns `0`.
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limits.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }

    // SAFETY: this is reached only after `getrlimit` returned `0`, so the OS
    // has initialized the `rlimit` value at `limits.as_mut_ptr()`.
    let limits = unsafe { limits.assume_init() };
    usize::try_from(limits.rlim_cur).ok()
}

#[cfg(unix)]
fn current_process_open_fd_count() -> Option<usize> {
    const FD_DIRS: &[&str] = &["/proc/self/fd", "/dev/fd"];

    for path in FD_DIRS {
        let Ok(entries) = fs::read_dir(path) else {
            continue;
        };
        return Some(entries.count().saturating_sub(1));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn fd_exhaustion_error(raw_os_error: i32) -> WebSocketError {
        WebSocketError::from(
            ewebsock::Error::from(std::io::Error::from_raw_os_error(raw_os_error))
                .with_context("Connect"),
        )
    }

    fn snapshot(open_fds: usize, soft_limit: usize) -> ProcessFdSnapshot {
        ProcessFdSnapshot::new(open_fds, soft_limit)
    }

    fn start_with_snapshot(
        gate: &mut FdPressureGate,
        open_websockets: usize,
        now: Instant,
        snapshot: ProcessFdSnapshot,
    ) -> RelayOpenContext {
        gate.start_batch_at_with_snapshot_reader(open_websockets, None, now, || Some(snapshot))
    }

    fn start_with_source(
        gate: &mut FdPressureGate,
        open_websockets: usize,
        now: Instant,
        source: PressureSource,
    ) -> RelayOpenContext {
        gate.start_batch_from_source(open_websockets, None, source, 0, now)
    }

    fn start_with_missing_snapshot(
        gate: &mut FdPressureGate,
        open_websockets: usize,
        now: Instant,
    ) -> RelayOpenContext {
        gate.start_batch_at_with_snapshot_reader(open_websockets, None, now, || None)
    }

    fn prioritized_demand(
        strongest_demand: RelayDemandPriority,
        request_count: usize,
    ) -> RelaySocketDemand {
        RelaySocketDemand::Prioritized(RelayConnectionPriority {
            strongest_demand,
            request_count,
        })
    }

    #[test]
    fn low_priority_admission_stops_at_watermark() {
        let mut gate = FdPressureGate::default();
        let batch = start_with_snapshot(&mut gate, 0, Instant::now(), snapshot(74, 100));

        assert_eq!(
            batch.decide(prioritized_demand(RelayDemandPriority::Opportunistic, 1)),
            RelayOpenDecision::Defer,
            "projected websocket opens should stop once they would cross the low-priority watermark",
        );
    }

    #[test]
    fn low_priority_throttle_uses_hysteresis() {
        let mut gate = FdPressureGate::default();
        let constrained = start_with_source(
            &mut gate,
            0,
            Instant::now(),
            PressureSource::ProcessFdSnapshot(snapshot(80, 100)),
        );
        assert_eq!(
            constrained.decide(prioritized_demand(RelayDemandPriority::Opportunistic, 1)),
            RelayOpenDecision::Defer
        );

        let still_throttled = start_with_source(
            &mut gate,
            0,
            Instant::now(),
            PressureSource::ProcessFdSnapshot(snapshot(70, 100)),
        );
        assert_eq!(
            still_throttled.decide(prioritized_demand(RelayDemandPriority::Opportunistic, 1)),
            RelayOpenDecision::Defer
        );

        let recovered = start_with_source(
            &mut gate,
            0,
            Instant::now(),
            PressureSource::ProcessFdSnapshot(snapshot(64, 100)),
        );
        assert_eq!(
            recovered.decide(prioritized_demand(RelayDemandPriority::Opportunistic, 1)),
            RelayOpenDecision::Open
        );
    }

    #[test]
    fn policy_refresh_deadline_tracks_snapshot_cache_expiry_and_hard_failure() {
        let mut gate = FdPressureGate::default();
        let now = Instant::now();
        let _batch = start_with_snapshot(&mut gate, 0, now, snapshot(80, 100));

        assert_eq!(
            gate.next_policy_refresh_deadline(now),
            Some(now + FD_SNAPSHOT_CACHE_TTL)
        );

        gate.enter_hard_failure_from_websocket_error_at(&fd_exhaustion_error(libc::EMFILE), now);
        assert_eq!(
            gate.next_policy_refresh_deadline(now),
            Some(now + FD_SNAPSHOT_CACHE_TTL)
        );

        assert_eq!(
            gate.next_policy_refresh_deadline(now + FD_SNAPSHOT_CACHE_TTL),
            Some(now + HARD_FAILURE_COOLDOWN)
        );
    }

    #[test]
    fn strong_demand_prefers_eviction_under_soft_constraint() {
        for demand in [
            prioritized_demand(RelayDemandPriority::Important, 1),
            prioritized_demand(RelayDemandPriority::Critical, usize::MAX),
        ] {
            let mut gate = FdPressureGate::default();
            let constrained = start_with_snapshot(&mut gate, 0, Instant::now(), snapshot(95, 100));

            assert_eq!(
                constrained.decide(demand),
                RelayOpenDecision::TryEvictThenOpen
            );
        }
    }

    #[test]
    fn fd_exhaustion_error_enters_hard_failure_mode() {
        let mut gate = FdPressureGate::default();
        let now = Instant::now();

        gate.enter_hard_failure_from_websocket_error_at(&fd_exhaustion_error(libc::EMFILE), now);
        let constrained = start_with_snapshot(&mut gate, 0, now, snapshot(0, 100));

        assert_eq!(
            constrained.decide(prioritized_demand(RelayDemandPriority::Important, 1)),
            RelayOpenDecision::RequireEviction,
            "hard fd failures should require eviction before prioritized opens proceed",
        );
    }

    #[test]
    fn hard_failure_mode_clears_after_cooldown() {
        let mut gate = FdPressureGate::default();
        let now = Instant::now();

        gate.enter_hard_failure_from_websocket_error_at(&fd_exhaustion_error(libc::EMFILE), now);
        let recovered = start_with_snapshot(
            &mut gate,
            0,
            now + HARD_FAILURE_COOLDOWN + Duration::from_secs(1),
            snapshot(0, 100),
        );

        assert_eq!(
            recovered.decide(prioritized_demand(RelayDemandPriority::Opportunistic, 1)),
            RelayOpenDecision::Open,
            "hard failure mode should expire after the cooldown elapses",
        );
    }

    #[test]
    fn successful_open_clears_hard_failure_mode() {
        let mut gate = FdPressureGate::default();
        let now = Instant::now();

        gate.enter_hard_failure_from_websocket_error_at(&fd_exhaustion_error(libc::EMFILE), now);
        gate.clear_hard_failure_on_open_success();
        let recovered = start_with_snapshot(&mut gate, 0, now, snapshot(0, 100));

        assert_eq!(
            recovered.decide(prioritized_demand(RelayDemandPriority::Opportunistic, 1)),
            RelayOpenDecision::Open,
            "a successful later open should clear hard-scarcity throttling immediately",
        );
    }

    #[test]
    fn missing_fd_telemetry_uses_fallback_connection_cap() {
        let mut gate = FdPressureGate::default();
        let constrained = start_with_missing_snapshot(&mut gate, 32, Instant::now());

        assert!(matches!(
            constrained.pressure_state(),
            PressureState::SoftConstrained(PressureSource::Fallback { .. })
        ));
        assert_eq!(
            constrained.decide(prioritized_demand(RelayDemandPriority::Opportunistic, 1)),
            RelayOpenDecision::Defer
        );
    }

    #[test]
    fn fd_snapshot_is_cached_within_sampling_cadence() {
        let mut gate = FdPressureGate::default();
        let now = Instant::now();
        let mut reads = 0usize;
        let constrained = gate.start_batch_at_with_snapshot_reader(0, None, now, || {
            reads = reads.saturating_add(1);
            Some(snapshot(80, 100))
        });
        assert!(matches!(
            constrained.pressure_state(),
            PressureState::SoftConstrained(PressureSource::ProcessFdSnapshot(ProcessFdSnapshot {
                open_fds: 80,
                soft_limit: 100
            }))
        ));
        assert_eq!(reads, 1);

        let cached = gate.start_batch_at_with_snapshot_reader(
            0,
            None,
            now + FD_SNAPSHOT_CACHE_TTL - Duration::from_millis(1),
            || {
                reads = reads.saturating_add(1);
                Some(snapshot(10, 100))
            },
        );
        assert!(matches!(
            cached.pressure_state(),
            PressureState::SoftConstrained(PressureSource::ProcessFdSnapshot(ProcessFdSnapshot {
                open_fds: 80,
                soft_limit: 100
            }))
        ));
        assert_eq!(reads, 1);

        let refreshed =
            gate.start_batch_at_with_snapshot_reader(0, None, now + FD_SNAPSHOT_CACHE_TTL, || {
                reads = reads.saturating_add(1);
                Some(snapshot(10, 100))
            });
        assert!(matches!(
            refreshed.pressure_state(),
            PressureState::Clear(PressureSource::ProcessFdSnapshot(ProcessFdSnapshot {
                open_fds: 10,
                soft_limit: 100
            }))
        ));
        assert_eq!(reads, 2);
    }

    #[test]
    fn cached_fd_snapshot_accounts_for_websocket_opens_since_sample() {
        let mut gate = FdPressureGate::default();
        let now = Instant::now();
        let low_priority = prioritized_demand(RelayDemandPriority::Opportunistic, 1);

        let mut reads = 0usize;
        let mut first_batch = gate.start_batch_at_with_snapshot_reader(10, None, now, || {
            reads = reads.saturating_add(1);
            Some(snapshot(70, 100))
        });
        for _ in 0..4 {
            assert_eq!(first_batch.decide(low_priority), RelayOpenDecision::Open);
            first_batch.record_socket_open();
        }
        assert_eq!(first_batch.decide(low_priority), RelayOpenDecision::Defer);

        let second_batch = gate.start_batch_at_with_snapshot_reader(
            14,
            None,
            now + FD_SNAPSHOT_CACHE_TTL - Duration::from_millis(1),
            || {
                reads = reads.saturating_add(1);
                Some(snapshot(10, 100))
            },
        );

        assert_eq!(reads, 1);
        assert_eq!(second_batch.projected_websocket_count(1), 15);
        assert_eq!(second_batch.decide(low_priority), RelayOpenDecision::Defer);
    }
}
