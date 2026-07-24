//! OS network-interface monitoring for realtime relay connection status.
//!
//! TCP keepalive ([`crate::relay::ws`]) catches a silently-dropped connection
//! within seconds, but that is still a timeout. When the *route* to a relay
//! disappears outright — a VPN/wireguard tunnel torn down, an interface
//! dropped — the OS knows immediately. [`NetworkWatcher`] surfaces those
//! interface-down events so the pool can mark the affected relay legs
//! disconnected the instant the path vanishes, rather than waiting on any
//! timeout.
//!
//! The watcher runs on a background tokio task and forwards the subnet of each
//! interface-down event through a channel the pool drains from its per-frame
//! keepalive pass.

use futures_util::StreamExt;
use if_watch::tokio::IfWatcher;
use if_watch::{IfEvent, IpNet};
use tokio::sync::mpsc::UnboundedReceiver;

/// Watches network interfaces and reports the subnets that go away.
pub struct NetworkWatcher {
    down_subnets: UnboundedReceiver<IpNet>,
}

impl NetworkWatcher {
    /// Start watching interfaces on a background tokio task. `wakeup` is invoked
    /// on each interface-down event so the host schedules a pool poll. Returns
    /// `None` if the platform watcher can't be created (the pool then falls back
    /// to TCP keepalive alone). Requires an active tokio runtime.
    pub fn spawn<W>(wakeup: W) -> Option<Self>
    where
        W: Fn() + Send + Sync + 'static,
    {
        let mut watcher = match IfWatcher::new() {
            Ok(watcher) => watcher,
            Err(err) => {
                tracing::warn!("network watcher unavailable, relying on tcp keepalive: {err}");
                return None;
            }
        };

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(event) = watcher.next().await {
                match event {
                    Ok(IfEvent::Down(subnet)) => {
                        if tx.send(subnet).is_err() {
                            break; // pool dropped the receiver
                        }
                        wakeup();
                    }
                    // An address coming up doesn't invalidate a live connection;
                    // reconnect logic handles bringing new relays online.
                    Ok(IfEvent::Up(_)) => {}
                    Err(err) => tracing::warn!("network watcher error: {err}"),
                }
            }
        });

        Some(Self { down_subnets: rx })
    }

    /// Drain the next interface-down subnet, if one has arrived. Non-blocking.
    pub fn try_recv(&mut self) -> Option<IpNet> {
        self.down_subnets.try_recv().ok()
    }

    /// Test-only: build a watcher backed by a caller-controlled channel so tests
    /// can inject interface-down events without a real network interface.
    #[cfg(test)]
    pub(crate) fn for_test() -> (Self, tokio::sync::mpsc::UnboundedSender<IpNet>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Self { down_subnets: rx }, tx)
    }
}
