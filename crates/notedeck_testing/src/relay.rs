//! Wrapper around `nostr_relay_builder::LocalRelay` that provides proper shutdown signaling.
//!
//! `LocalRelay::shutdown()` signals the background listener task to stop but does not
//! wait for it to exit. This wrapper waits for the listener task to actually exit
//! by polling the relay's socket.

use nostr_relay_builder::LocalRelay;
use std::time::Duration;

/// Maximum time to wait for relay shutdown.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Interval between shutdown completion checks.
const SHUTDOWN_CHECK_INTERVAL: Duration = Duration::from_millis(5);

#[allow(async_fn_in_trait)]
/// Extension trait for `LocalRelay` that adds proper shutdown waiting.
pub trait LocalRelayExt {
    /// Calls shutdown and waits for the relay to fully exit.
    ///
    /// This method signals the relay's listener task to stop and then polls
    /// the relay's socket until it no longer accepts connections. When the
    /// socket returns ECONNREFUSED, the listener task has exited.
    async fn shutdown_and_wait(self);
}

impl LocalRelayExt for LocalRelay {
    async fn shutdown_and_wait(self) {
        let addr = self
            .url()
            .trim_start_matches("ws://")
            .trim_end_matches('/')
            .to_owned();

        // Signal the listener task to shut down
        self.shutdown();

        // Wait for the listener task to actually exit by polling the socket.
        // When the task exits, it drops the TcpListener, and the OS closes
        // the socket. Subsequent connection attempts will get ECONNREFUSED.
        let deadline = std::time::Instant::now() + SHUTDOWN_TIMEOUT;

        loop {
            match tokio::net::TcpStream::connect(addr.as_str()).await {
                Ok(_) => {
                    // Connection succeeded - listener is still running. Wait and retry.
                    tokio::time::sleep(SHUTDOWN_CHECK_INTERVAL).await;
                }
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                    // Connection refused - listener has stopped accepting.
                    return;
                }
                Err(e) => {
                    // Unexpected error - log and continue polling.
                    tracing::debug!("Shutdown poll error: {e}");
                    tokio::time::sleep(SHUTDOWN_CHECK_INTERVAL).await;
                }
            }

            if std::time::Instant::now() >= deadline {
                tracing::warn!("Relay shutdown timed out after {:?}", SHUTDOWN_TIMEOUT);
                return;
            }
        }
    }
}
