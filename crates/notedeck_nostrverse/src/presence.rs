//! Coarse presence via nostr events (kind 10555).
//!
//! Publishes only on meaningful position change (with 1s minimum gap),
//! plus a keep-alive heartbeat every 60s to maintain room presence.
//! Not intended for smooth real-time movement sync.

use enostr::{FilledKeypair, Pubkey};
use glam::Vec3;
use nostrdb::Ndb;

use crate::{nostr_events, room_state::RoomUser, subscriptions::PresenceSubscription};

/// Minimum position change (distance) to trigger a publish.
const POSITION_THRESHOLD: f32 = 0.5;

/// Minimum seconds between publishes even when moving.
const MIN_PUBLISH_GAP: f64 = 1.0;

/// Keep-alive interval: publish even when idle to stay visible.
const KEEPALIVE_INTERVAL: f64 = 60.0;

/// Seconds without a heartbeat before a remote user is considered gone.
const STALE_TIMEOUT: f64 = 90.0;

/// How often to check for stale users (seconds).
const EXPIRY_CHECK_INTERVAL: f64 = 10.0;

/// Minimum speed to consider "moving" (units/s). Below this, velocity is zeroed.
const MIN_SPEED: f32 = 0.1;

/// Direction change threshold (dot product). cos(30°) ≈ 0.866.
/// If the normalized velocity direction changes by more than ~30°, publish.
const DIRECTION_CHANGE_THRESHOLD: f32 = 0.866;

/// Publishes local user presence as kind 10555 events.
///
/// Only publishes when position or velocity changes meaningfully, plus periodic
/// keep-alive to maintain room presence. Includes velocity for dead reckoning.
pub struct PresencePublisher {
    /// Last position we published
    last_position: Vec3,
    /// Last velocity we published
    last_velocity: Vec3,
    /// Monotonic time of last publish
    last_publish_time: f64,
    /// Previous position sample (for computing velocity)
    prev_position: Vec3,
    /// Time of previous position sample
    prev_position_time: f64,
    /// Whether we've published at least once
    published_once: bool,
}

impl PresencePublisher {
    pub fn new() -> Self {
        Self {
            last_position: Vec3::ZERO,
            last_velocity: Vec3::ZERO,
            last_publish_time: 0.0,
            prev_position: Vec3::ZERO,
            prev_position_time: 0.0,
            published_once: false,
        }
    }

    /// Compute instantaneous velocity from position samples.
    fn compute_velocity(&self, position: Vec3, now: f64) -> Vec3 {
        let dt = now - self.prev_position_time;
        if dt < 0.01 {
            return self.last_velocity;
        }
        let vel = (position - self.prev_position) / dt as f32;
        if vel.length() < MIN_SPEED {
            Vec3::ZERO
        } else {
            vel
        }
    }

    /// Check whether a publish should happen (without side effects).
    fn should_publish(&self, position: Vec3, velocity: Vec3, now: f64) -> bool {
        // Always publish the first time
        if !self.published_once {
            return true;
        }

        let elapsed = now - self.last_publish_time;

        // Rate limit: never more than once per second
        if elapsed < MIN_PUBLISH_GAP {
            return false;
        }

        // Publish if position changed meaningfully
        if self.last_position.distance(position) > POSITION_THRESHOLD {
            return true;
        }

        // Publish on start/stop transitions
        let was_moving = self.last_velocity.length() > MIN_SPEED;
        let is_moving = velocity.length() > MIN_SPEED;
        if was_moving != is_moving {
            return true;
        }

        // Publish on significant direction change while moving
        if was_moving && is_moving {
            let old_dir = self.last_velocity.normalize();
            let new_dir = velocity.normalize();
            if old_dir.dot(new_dir) < DIRECTION_CHANGE_THRESHOLD {
                return true;
            }
        }

        // Keep-alive: publish periodically even when idle
        elapsed >= KEEPALIVE_INTERVAL
    }

    /// Record that a publish happened (update internal state).
    fn record_publish(&mut self, position: Vec3, velocity: Vec3, now: f64) {
        self.last_position = position;
        self.last_velocity = velocity;
        self.last_publish_time = now;
        self.published_once = true;
    }

    /// Maybe publish a presence heartbeat.
    /// Returns the ClientMessage if published (for optional relay forwarding).
    pub fn maybe_publish(
        &mut self,
        ndb: &Ndb,
        kp: FilledKeypair,
        room_naddr: &str,
        position: Vec3,
        now: f64,
    ) -> Option<enostr::ClientMessage> {
        let velocity = self.compute_velocity(position, now);

        // Always update position sample for velocity computation
        self.prev_position = position;
        self.prev_position_time = now;

        if !self.should_publish(position, velocity, now) {
            return None;
        }

        let builder = nostr_events::build_presence_event(room_naddr, position, velocity);
        let result = nostr_events::ingest_event(builder, ndb, kp);

        self.record_publish(position, velocity, now);
        result.map(|(msg, _id)| msg)
    }
}

/// Poll for presence events and update the user list.
///
/// Returns true if any users were added or updated.
pub fn poll_presence(
    sub: &PresenceSubscription,
    ndb: &Ndb,
    room_naddr: &str,
    self_pubkey: &Pubkey,
    users: &mut Vec<RoomUser>,
    now: f64,
) -> bool {
    let txn = nostrdb::Transaction::new(ndb).expect("txn");
    let notes = sub.poll(ndb, &txn);
    let mut changed = false;

    for note in &notes {
        // Filter to our space
        let Some(event_space) = nostr_events::get_presence_space(note) else {
            continue;
        };
        if event_space != room_naddr {
            continue;
        }

        let Some(position) = nostr_events::parse_presence_position(note) else {
            continue;
        };

        let pubkey = Pubkey::new(*note.pubkey());

        // Skip our own presence events
        if &pubkey == self_pubkey {
            continue;
        }

        let velocity = nostr_events::parse_presence_velocity(note);

        // Update or insert user
        if let Some(user) = users.iter_mut().find(|u| u.pubkey == pubkey) {
            // Update authoritative state; preserve display_position for smooth lerp
            user.position = position;
            user.velocity = velocity;
            user.update_time = now;
            user.last_seen = now;
        } else {
            let mut user = RoomUser::new(pubkey, "anon".to_string(), position);
            user.velocity = velocity;
            user.display_position = position; // snap on first appearance
            user.update_time = now;
            user.last_seen = now;
            users.push(user);
        }
        changed = true;
    }

    changed
}

/// Remove users who haven't sent a heartbeat recently.
/// Throttled to only run every EXPIRY_CHECK_INTERVAL seconds.
pub struct PresenceExpiry {
    last_check: f64,
}

impl PresenceExpiry {
    pub fn new() -> Self {
        Self { last_check: 0.0 }
    }

    /// Maybe expire stale users. Returns the number removed (0 if check was skipped).
    pub fn maybe_expire(&mut self, users: &mut Vec<RoomUser>, now: f64) -> usize {
        if now - self.last_check < EXPIRY_CHECK_INTERVAL {
            return 0;
        }
        self.last_check = now;
        let before = users.len();
        users.retain(|u| u.is_self || (now - u.last_seen) < STALE_TIMEOUT);
        before - users.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expiry_throttle_and_cleanup() {
        let pk1 = Pubkey::new([1; 32]);
        let pk2 = Pubkey::new([2; 32]);
        let pk_self = Pubkey::new([3; 32]);

        let mut users = vec![
            {
                let mut u = RoomUser::new(pk_self, "me".to_string(), Vec3::ZERO);
                u.is_self = true;
                u.last_seen = 0.0; // stale but self — should survive
                u
            },
            {
                let mut u = RoomUser::new(pk1, "alice".to_string(), Vec3::ZERO);
                u.last_seen = 80.0; // fresh (within 90s timeout)
                u
            },
            {
                let mut u = RoomUser::new(pk2, "bob".to_string(), Vec3::ZERO);
                u.last_seen = 1.0; // stale (>90s ago)
                u
            },
        ];

        let mut expiry = PresenceExpiry::new();

        // First call at t=5 — too soon (< 10s from init at 0.0), skipped
        assert_eq!(expiry.maybe_expire(&mut users, 5.0), 0);
        assert_eq!(users.len(), 3); // no one removed

        // At t=100 — enough time, bob is stale
        let removed = expiry.maybe_expire(&mut users, 100.0);
        assert_eq!(removed, 1);
        assert_eq!(users.len(), 2);
        assert!(users.iter().any(|u| u.is_self));
        assert!(users.iter().any(|u| u.display_name == "alice"));

        // Immediately again at t=101 — throttled, skipped
        assert_eq!(expiry.maybe_expire(&mut users, 101.0), 0);
    }

    #[test]
    fn test_publisher_first_publish() {
        let pub_ = PresencePublisher::new();
        // First publish should always happen
        assert!(pub_.should_publish(Vec3::ZERO, Vec3::ZERO, 0.0));
    }

    #[test]
    fn test_publisher_no_spam_when_idle() {
        let mut pub_ = PresencePublisher::new();
        pub_.record_publish(Vec3::ZERO, Vec3::ZERO, 0.0);

        // Idle at same position — should NOT publish at 1s, 5s, 10s, 30s
        assert!(!pub_.should_publish(Vec3::ZERO, Vec3::ZERO, 1.0));
        assert!(!pub_.should_publish(Vec3::ZERO, Vec3::ZERO, 5.0));
        assert!(!pub_.should_publish(Vec3::ZERO, Vec3::ZERO, 10.0));
        assert!(!pub_.should_publish(Vec3::ZERO, Vec3::ZERO, 30.0));

        // Keep-alive triggers at 60s
        assert!(pub_.should_publish(Vec3::ZERO, Vec3::ZERO, 60.1));
    }

    #[test]
    fn test_publisher_on_movement() {
        let mut pub_ = PresencePublisher::new();
        pub_.record_publish(Vec3::ZERO, Vec3::ZERO, 0.0);

        // Small movement below threshold — no publish
        assert!(!pub_.should_publish(Vec3::new(0.1, 0.0, 0.0), Vec3::ZERO, 2.0));

        // Significant movement — publish
        assert!(pub_.should_publish(Vec3::new(5.0, 0.0, 0.0), Vec3::ZERO, 2.0));

        // But rate limited: can't publish again within 1s
        pub_.record_publish(Vec3::new(5.0, 0.0, 0.0), Vec3::ZERO, 2.0);
        assert!(!pub_.should_publish(Vec3::new(10.0, 0.0, 0.0), Vec3::ZERO, 2.5));

        // After 1s gap, can publish again
        assert!(pub_.should_publish(Vec3::new(10.0, 0.0, 0.0), Vec3::ZERO, 3.1));
    }

    #[test]
    fn test_publisher_velocity_start_stop() {
        let mut pub_ = PresencePublisher::new();
        let pos = Vec3::new(1.0, 0.0, 0.0);
        pub_.record_publish(pos, Vec3::ZERO, 0.0);

        // Start moving — should trigger (velocity went from zero to non-zero)
        let vel = Vec3::new(3.0, 0.0, 0.0);
        assert!(pub_.should_publish(pos, vel, 2.0));
        pub_.record_publish(pos, vel, 2.0);

        // Stop moving — should trigger (velocity went from non-zero to zero)
        assert!(pub_.should_publish(pos, Vec3::ZERO, 3.5));
    }

    #[test]
    fn test_publisher_velocity_direction_change() {
        let mut pub_ = PresencePublisher::new();
        let pos = Vec3::new(1.0, 0.0, 0.0);
        let vel_east = Vec3::new(3.0, 0.0, 0.0);
        pub_.record_publish(pos, vel_east, 0.0);

        // Small direction change (still mostly east) — no publish
        let vel_slight = Vec3::new(3.0, 0.0, 0.5);
        assert!(!pub_.should_publish(pos, vel_slight, 2.0));

        // Large direction change (east → north, 90 degrees) — should publish
        let vel_north = Vec3::new(0.0, 0.0, 3.0);
        assert!(pub_.should_publish(pos, vel_north, 2.0));
    }

    #[test]
    fn test_compute_velocity() {
        let mut pub_ = PresencePublisher::new();
        pub_.prev_position = Vec3::ZERO;
        pub_.prev_position_time = 0.0;

        // 5 units in 1 second = 5 units/s
        let vel = pub_.compute_velocity(Vec3::new(5.0, 0.0, 0.0), 1.0);
        assert!((vel.x - 5.0).abs() < 0.01);

        // Very small movement → zeroed (below MIN_SPEED)
        pub_.prev_position = Vec3::ZERO;
        pub_.prev_position_time = 0.0;
        let vel = pub_.compute_velocity(Vec3::new(0.01, 0.0, 0.0), 1.0);
        assert_eq!(vel, Vec3::ZERO);
    }
}
