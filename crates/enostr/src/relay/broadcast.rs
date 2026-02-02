use crate::{
    relay::{MulticastRelay, UnownedRelay, WebsocketRelay},
    ClientMessage, EventClientMessage, RelayStatus,
};

/// BroadcastCache stores queued events for relays that are temporarily disconnected.
#[derive(Default)]
pub struct BroadcastCache {
    to_send: Vec<EventClientMessage>,
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

                self.cache.to_send.retain(|m| multicast.send(m).is_err());
            }
        }
    }
}
