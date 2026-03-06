use std::time::{Duration, SystemTime};

/// Raw `limitation` object from a relay NIP-11 document.
///
/// Outbox code decides which fields matter for runtime behavior.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Nip11LimitationsRaw {
    pub max_message_length: Option<i64>,
    pub max_subscriptions: Option<i64>,
    pub max_filters: Option<i64>,
    pub max_limit: Option<i64>,
    pub max_subid_length: Option<i64>,
    pub max_event_tags: Option<i64>,
    pub max_content_length: Option<i64>,
    pub min_pow_difficulty: Option<i64>,
    pub auth_required: Option<bool>,
    pub payment_required: Option<bool>,
    pub created_at_lower_limit: Option<i64>,
    pub created_at_upper_limit: Option<i64>,
}

/// Fetch work item requested by outbox for a specific relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nip11FetchRequest {
    pub relay: crate::relay::NormRelayUrl,
    pub attempt: u32,
    pub requested_at: SystemTime,
}

/// Result of applying a raw NIP-11 response to a relay coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nip11ApplyOutcome {
    Applied,
    Unchanged,
    RelayUnknown,
}

/// Per-relay NIP-11 fetch lifecycle and backoff state.
#[derive(Debug, Clone)]
pub struct Nip11FetchLifecycle {
    pending: bool,
    in_flight: bool,
    attempt: u32,
    last_success: Option<SystemTime>,
    last_error: Option<String>,
    next_retry_at: Option<SystemTime>,
}

impl Default for Nip11FetchLifecycle {
    fn default() -> Self {
        Self {
            pending: true,
            in_flight: false,
            attempt: 0,
            last_success: None,
            last_error: None,
            next_retry_at: None,
        }
    }
}

impl Nip11FetchLifecycle {
    fn update_pending_from_schedule(&mut self, now: SystemTime) {
        let Some(next) = self.next_retry_at else {
            return;
        };

        if now >= next {
            self.pending = true;
        }
    }

    pub(crate) fn ready_to_fetch(&mut self, now: SystemTime) -> bool {
        self.update_pending_from_schedule(now);

        if self.in_flight || !self.pending {
            return false;
        }

        match self.next_retry_at {
            Some(next) => now >= next,
            None => true,
        }
    }

    pub(crate) fn mark_dispatched(&mut self) -> u32 {
        self.in_flight = true;
        self.pending = false;
        self.attempt = self.attempt.saturating_add(1);
        self.attempt
    }

    pub(crate) fn mark_success(&mut self, at: SystemTime, refresh_after: Duration) {
        self.in_flight = false;
        self.pending = false;
        self.attempt = 0;
        self.last_success = Some(at);
        self.last_error = None;
        self.next_retry_at = at.checked_add(refresh_after);
    }

    pub(crate) fn mark_failure(&mut self, at: SystemTime, error: String, retry_after: Duration) {
        self.in_flight = false;
        self.pending = true;
        self.last_error = Some(error);
        self.next_retry_at = at.checked_add(retry_after);
    }

    pub(crate) fn attempt(&self) -> u32 {
        self.attempt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nip11_lifecycle_defaults_to_pending() {
        let lifecycle = Nip11FetchLifecycle::default();
        assert!(lifecycle.pending);
        assert!(!lifecycle.in_flight);
        assert_eq!(lifecycle.attempt, 0);
    }

    #[test]
    fn nip11_lifecycle_dispatch_and_success() {
        let mut lifecycle = Nip11FetchLifecycle::default();
        assert!(lifecycle.ready_to_fetch(SystemTime::now()));
        assert_eq!(lifecycle.mark_dispatched(), 1);
        assert!(lifecycle.in_flight);
        assert!(!lifecycle.pending);

        let now = SystemTime::now();
        lifecycle.mark_success(now, Duration::from_secs(60));
        assert!(!lifecycle.in_flight);
        assert!(!lifecycle.pending);
        assert_eq!(lifecycle.attempt, 0);
        assert!(lifecycle.last_success.is_some());
        assert!(lifecycle.last_error.is_none());
    }

    #[test]
    fn nip11_lifecycle_failure_sets_pending() {
        let mut lifecycle = Nip11FetchLifecycle::default();
        let _ = lifecycle.mark_dispatched();
        let now = SystemTime::now();
        lifecycle.mark_failure(now, "boom".to_owned(), Duration::from_secs(10));
        assert!(!lifecycle.in_flight);
        assert!(lifecycle.pending);
        assert_eq!(lifecycle.last_error.as_deref(), Some("boom"));
    }
}
