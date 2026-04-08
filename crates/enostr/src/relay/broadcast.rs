use std::time::Duration;

use crate::{
    relay::{backoff::FlushBackoff, MulticastRelay, UnownedRelay, WebsocketRelay},
    ClientMessage, EventClientMessage, RelayStatus,
};

const MAX_MULTICAST_FLUSH_BACKOFF: Duration = Duration::from_secs(60);

/// BroadcastCache stores queued events for relays that are temporarily disconnected.
#[derive(Default)]
pub struct BroadcastCache {
    to_send: Vec<EventClientMessage>,
    pub(crate) flush_backoff: Option<FlushBackoff>,
}

#[cfg(test)]
impl BroadcastCache {
    /// Returns the number of queued multicast events waiting for a retry.
    pub(crate) fn queued_len(&self) -> usize {
        self.to_send.len()
    }

    /// Returns whether the multicast retry queue is empty.
    pub(crate) fn queue_is_empty(&self) -> bool {
        self.to_send.is_empty()
    }
}

/// BroadcastRelay sends events to either a websocket relay or the multicast relay
/// while handling retries via the shared cache.
pub struct BroadcastRelay<'a> {
    relay: Option<UnownedRelay<'a>>,
    cache: &'a mut BroadcastCache,
}

impl<'a> BroadcastRelay<'a> {
    pub fn websocket(
        websocket: Option<&'a mut WebsocketRelay>,
        cache: &'a mut BroadcastCache,
    ) -> Self {
        Self {
            relay: websocket.map(UnownedRelay::Websocket),
            cache,
        }
    }

    pub fn multicast(
        multicast: Option<&'a mut MulticastRelay>,
        cache: &'a mut BroadcastCache,
    ) -> Self {
        Self {
            relay: multicast.map(UnownedRelay::Multicast),
            cache,
        }
    }

    pub fn broadcast(&mut self, msg: EventClientMessage) {
        let Some(relay) = &mut self.relay else {
            self.cache.to_send.push(msg);
            return;
        };

        match relay {
            UnownedRelay::Websocket(websocket_relay) => {
                if !websocket_relay.is_connected() {
                    self.cache.to_send.push(msg);
                    return;
                }

                websocket_relay.conn.send(&ClientMessage::Event(msg));
            }
            UnownedRelay::Multicast(multicast) => {
                // Always queue if we're not connected.
                if multicast.status() != RelayStatus::Connected {
                    self.cache.to_send.push(msg.clone());
                    return;
                }

                if multicast.send(&msg).is_err() {
                    self.cache.to_send.push(msg.clone());
                    if self.cache.flush_backoff.is_none() {
                        self.cache.flush_backoff =
                            Some(FlushBackoff::new(MAX_MULTICAST_FLUSH_BACKOFF));
                    }
                }
            }
        }
    }

    #[profiling::function]
    pub fn try_flush_queue(&mut self) {
        let Some(relay) = &mut self.relay else {
            return;
        };

        match relay {
            UnownedRelay::Websocket(websocket) => {
                if !websocket.is_connected() || self.cache.to_send.is_empty() {
                    return;
                }

                for item in self.cache.to_send.drain(..) {
                    websocket.conn.send(&ClientMessage::Event(item));
                }
            }
            UnownedRelay::Multicast(multicast) => {
                if multicast.status() != RelayStatus::Connected || self.cache.to_send.is_empty() {
                    return;
                }

                if let Some(backoff) = &self.cache.flush_backoff {
                    if !backoff.is_elapsed() {
                        return;
                    }
                }

                let msgs_before_flush = self.cache.to_send.len();
                self.cache.to_send.retain(|m| multicast.send(m).is_err());
                let msgs_remaining = self.cache.to_send.len();

                if msgs_remaining == 0 {
                    self.cache.flush_backoff = None;
                } else if msgs_remaining == msgs_before_flush {
                    match &mut self.cache.flush_backoff {
                        Some(backoff) => backoff.escalate(),
                        None => {
                            self.cache.flush_backoff =
                                Some(FlushBackoff::new(MAX_MULTICAST_FLUSH_BACKOFF))
                        }
                    }
                } else {
                    // Partial progress: start fresh backoff
                    self.cache.flush_backoff = Some(FlushBackoff::new(MAX_MULTICAST_FLUSH_BACKOFF));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::multicast::MulticastRelay;
    use mio::net::UdpSocket;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};

    fn test_multicast_relay() -> MulticastRelay {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
        let socket = UdpSocket::bind(addr).unwrap();
        let multicast_addr = SocketAddrV4::new(Ipv4Addr::new(239, 19, 88, 1), 9797);
        MulticastRelay::new(multicast_addr, socket, Ipv4Addr::UNSPECIFIED)
    }

    /// An oversized message that exceeds the UDP maximum datagram size
    /// (~65KB), causing multicast send to fail with EMSGSIZE.
    fn oversized_msg() -> EventClientMessage {
        EventClientMessage {
            note_json: "x".repeat(70_000),
        }
    }

    #[test]
    fn failed_multicast_send_activates_backoff() {
        let mut cache = BroadcastCache::default();
        let mut multicast = test_multicast_relay();

        BroadcastRelay::multicast(Some(&mut multicast), &mut cache).broadcast(oversized_msg());

        assert_eq!(
            cache.to_send.len(),
            1,
            "failed send should queue the message"
        );
        assert!(
            cache.flush_backoff.is_some(),
            "failed send should activate backoff"
        );
    }
}
