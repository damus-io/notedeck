use ewebsock::{WsEvent, WsMessage};
use mio::net::UdpSocket;
use std::io;
use std::net::IpAddr;
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::relay::{backoff::FlushBackoff, RawEventData, RelayImplType};
use crate::{EventClientMessage, RelayStatus, Result};
use std::net::Ipv4Addr;
use tracing::{debug, error};

const MAX_MULTICAST_FLUSH_BACKOFF: Duration = Duration::from_secs(60);

pub struct MulticastRelay {
    last_join: Instant,
    rejoin_interval: Duration,
    status: RelayStatus,
    address: SocketAddrV4,
    socket: UdpSocket,
    interface: Ipv4Addr,
    poller_stop: Arc<AtomicBool>,
    poller_handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for MulticastRelay {
    fn drop(&mut self) {
        self.poller_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.poller_handle.take() {
            handle.join().ok();
        }
    }
}

impl MulticastRelay {
    pub fn new(address: SocketAddrV4, socket: UdpSocket, interface: Ipv4Addr) -> Self {
        let last_join = Instant::now();
        let status = RelayStatus::Connected;
        MulticastRelay {
            status,
            address,
            socket,
            interface,
            last_join,
            rejoin_interval: Duration::from_secs(200),
            poller_stop: Arc::new(AtomicBool::new(false)),
            poller_handle: None,
        }
    }

    /// Multicast seems to fail every 260 seconds. We force a rejoin every 200 seconds or
    /// so to ensure we are always in the group
    pub fn rejoin(&mut self) -> Result<()> {
        self.last_join = Instant::now();
        self.status = RelayStatus::Disconnected;
        self.socket
            .leave_multicast_v4(self.address.ip(), &self.interface)?;
        self.socket
            .join_multicast_v4(self.address.ip(), &self.interface)?;
        self.status = RelayStatus::Connected;
        Ok(())
    }

    pub fn should_rejoin(&self) -> bool {
        self.should_rejoin_at(Instant::now())
    }

    pub fn should_rejoin_at(&self, now: Instant) -> bool {
        (now - self.last_join) >= self.rejoin_interval
    }

    pub fn next_rejoin_deadline(&self) -> Instant {
        self.last_join + self.rejoin_interval
    }

    pub fn set_rejoin_interval(&mut self, interval: Duration) {
        self.rejoin_interval = interval;
    }

    pub fn try_recv(&self) -> Option<WsEvent> {
        let mut buffer = [0u8; 65535];
        // Read the size header
        match self.socket.recv_from(&mut buffer) {
            Ok((size, src)) => {
                let parsed_size = u32::from_be_bytes(buffer[0..4].try_into().ok()?) as usize;
                debug!("multicast: read size {} from start of header", size - 4);

                if size != parsed_size + 4 {
                    error!(
                        "multicast: partial data received: expected {}, got {}",
                        parsed_size, size
                    );
                    return None;
                }

                let text = String::from_utf8_lossy(&buffer[4..size]);
                debug!("multicast: received {} bytes from {}: {}", size, src, &text);
                Some(WsEvent::Message(WsMessage::Text(text.to_string())))
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                // No data available, continue
                None
            }
            Err(e) => {
                error!("multicast: error receiving data: {}", e);
                None
            }
        }
    }

    pub fn send(&self, msg: &EventClientMessage) -> Result<()> {
        let json = msg.to_json();
        let len = json.len();

        let mut buf: Vec<u8> = Vec::with_capacity(4 + len);

        // Write the length of the message as 4 bytes (big-endian)
        buf.extend_from_slice(&(len as u32).to_be_bytes());

        // Append the JSON message bytes
        buf.extend_from_slice(json.as_bytes());

        let json_msg = msg.to_json();

        let end = floor_char_boundary(&json_msg, 128);
        debug!("writing to multicast relay: {}", &json_msg[..end]);
        self.socket.send_to(&buf, SocketAddr::V4(self.address))?;
        Ok(())
    }

    pub fn status(&self) -> RelayStatus {
        self.status
    }
}

#[inline]
pub fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        s.len()
    } else {
        let lower_bound = index.saturating_sub(3);
        let new_index = s.as_bytes()[lower_bound..=index]
            .iter()
            .rposition(|b| is_utf8_char_boundary(*b));

        // SAFETY: we know that the character boundary will be within four bytes
        unsafe { lower_bound + new_index.unwrap_unchecked() }
    }
}

#[inline]
fn is_utf8_char_boundary(c: u8) -> bool {
    // This is bit magic equivalent to: b < 128 || b >= 192
    (c as i8) >= -0x40
}

pub fn setup_multicast_relay(
    wakeup: impl Fn() + Send + Sync + Clone + 'static,
) -> Result<MulticastRelay> {
    use mio::{Events, Interest, Poll, Token};

    let port = 9797;
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), port);
    let multicast_ip = Ipv4Addr::new(239, 19, 88, 1);

    let mut socket = UdpSocket::bind(address)?;
    let interface = Ipv4Addr::UNSPECIFIED;
    let multicast_address = SocketAddrV4::new(multicast_ip, port);

    socket.join_multicast_v4(&multicast_ip, &interface)?;

    let mut poll = Poll::new()?;
    poll.registry().register(
        &mut socket,
        Token(0),
        Interest::READABLE | Interest::WRITABLE,
    )?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();

    let handle = std::thread::Builder::new()
        .name("multicast-poll".to_string())
        .spawn(move || {
            let mut events = Events::with_capacity(1);
            let poll_timeout = Some(Duration::from_millis(100));
            loop {
                if stop_thread.load(Ordering::Relaxed) {
                    return;
                }
                if let Err(err) = poll.poll(&mut events, poll_timeout) {
                    error!("multicast socket poll error: {err}. ending multicast poller.");
                    return;
                }
                if !events.is_empty() {
                    wakeup();
                }
                std::thread::yield_now();
            }
        })
        .map_err(|e| crate::Error::Generic(e.to_string()))?;

    let mut relay = MulticastRelay::new(multicast_address, socket, interface);
    relay.poller_stop = stop;
    relay.poller_handle = Some(handle);

    Ok(relay)
}
/// MulticastTransportRuntime lazily initializes the multicast connection and buffers
/// outbound events until a connection is available.
pub(crate) struct MulticastTransportRuntime {
    multicast: Option<MulticastRelay>,
    to_send: Vec<EventClientMessage>,
    flush_backoff: Option<FlushBackoff>,
    rejoin_interval: Duration,
}

impl Default for MulticastTransportRuntime {
    fn default() -> Self {
        Self {
            multicast: None,
            to_send: Vec::new(),
            flush_backoff: None,
            rejoin_interval: Duration::from_secs(200),
        }
    }
}

impl MulticastTransportRuntime {
    pub(crate) fn is_setup(&self) -> bool {
        self.multicast.is_some()
    }

    pub(crate) fn try_setup_fn(&mut self, wakeup: impl Fn() + Send + Sync + Clone + 'static) {
        let Ok(multicast) = setup_multicast_relay(wakeup) else {
            return;
        };
        let mut multicast = multicast;
        multicast.set_rejoin_interval(self.rejoin_interval);
        self.multicast = Some(multicast);
    }

    pub(crate) fn next_maintenance_deadline(&self) -> Option<Instant> {
        let multicast = self.multicast.as_ref()?;
        let rejoin_deadline = multicast.next_rejoin_deadline();
        let flush_deadline = self.next_flush_deadline(multicast);

        Some(
            flush_deadline
                .map(|deadline| deadline.min(rejoin_deadline))
                .unwrap_or(rejoin_deadline),
        )
    }

    fn next_flush_deadline(&self, multicast: &MulticastRelay) -> Option<Instant> {
        if self.to_send.is_empty() || multicast.status() != RelayStatus::Connected {
            return None;
        }

        self.flush_backoff
            .as_ref()
            .map(FlushBackoff::retry_deadline)
            .or_else(|| Some(Instant::now()))
    }

    pub(crate) fn broadcast(&mut self, msg: EventClientMessage) {
        let Some(multicast) = &mut self.multicast else {
            self.to_send.push(msg);
            return;
        };

        if multicast.status() != RelayStatus::Connected {
            self.to_send.push(msg);
            return;
        }

        if multicast.send(&msg).is_err() {
            self.to_send.push(msg);
            if self.flush_backoff.is_none() {
                self.flush_backoff = Some(FlushBackoff::new(MAX_MULTICAST_FLUSH_BACKOFF));
            }
        }
    }

    #[profiling::function]
    pub(crate) fn try_recv<F>(&mut self, mut process: F) -> usize
    where
        for<'a> F: FnMut(RawEventData<'a>),
    {
        self.maintain();
        self.try_flush_queue();

        let Some(multicast) = &mut self.multicast else {
            return 0;
        };

        let mut received = 0;
        while let Some(event) = multicast.try_recv() {
            let WsEvent::Message(WsMessage::Text(text)) = event else {
                continue;
            };

            process(RawEventData {
                url: "multicast",
                event_json: &text,
                relay_type: RelayImplType::Multicast,
            });
            received += 1;
        }
        received
    }

    fn maintain(&mut self) {
        let Some(multicast) = &mut self.multicast else {
            return;
        };

        if multicast.should_rejoin() {
            if let Err(e) = multicast.rejoin() {
                tracing::error!("multicast: rejoin error: {e}");
            } else {
                self.flush_backoff = None;
            }
        }
    }

    fn try_flush_queue(&mut self) {
        let Some(multicast) = &mut self.multicast else {
            return;
        };
        if multicast.status() != RelayStatus::Connected || self.to_send.is_empty() {
            return;
        }
        if let Some(backoff) = &self.flush_backoff {
            if !backoff.is_elapsed() {
                return;
            }
        }

        let msgs_before_flush = self.to_send.len();
        self.to_send.retain(|msg| multicast.send(msg).is_err());
        let msgs_remaining = self.to_send.len();

        if msgs_remaining == 0 {
            self.flush_backoff = None;
        } else if msgs_remaining == msgs_before_flush {
            match &mut self.flush_backoff {
                Some(backoff) => backoff.escalate(),
                None => self.flush_backoff = Some(FlushBackoff::new(MAX_MULTICAST_FLUSH_BACKOFF)),
            }
        } else {
            self.flush_backoff = Some(FlushBackoff::new(MAX_MULTICAST_FLUSH_BACKOFF));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MulticastRelay, MulticastTransportRuntime, MAX_MULTICAST_FLUSH_BACKOFF};
    use crate::{relay::backoff::FlushBackoff, EventClientMessage, RelayImplType, RelayStatus};
    use mio::net::UdpSocket;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket as StdUdpSocket};
    use std::time::{Duration, Instant};

    fn test_multicast_relay() -> MulticastRelay {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
        let socket = UdpSocket::bind(addr).unwrap();
        let multicast_addr = SocketAddrV4::new(Ipv4Addr::new(239, 19, 88, 1), 9797);
        socket
            .join_multicast_v4(multicast_addr.ip(), &Ipv4Addr::UNSPECIFIED)
            .unwrap();
        MulticastRelay::new(multicast_addr, socket, Ipv4Addr::UNSPECIFIED)
    }

    #[test]
    fn multicast_rejoin_interval_controls_rejoin_threshold() {
        let mut relay = test_multicast_relay();
        let joined_at = Instant::now();
        relay.last_join = joined_at;
        relay.set_rejoin_interval(Duration::from_millis(20));

        assert!(!relay.should_rejoin_at(joined_at + Duration::from_millis(10)));
        assert!(relay.should_rejoin_at(joined_at + Duration::from_millis(25)));
    }

    fn encode_text_frame(text: &str) -> Vec<u8> {
        let mut frame = Vec::with_capacity(4 + text.len());
        frame.extend_from_slice(&(text.len() as u32).to_be_bytes());
        frame.extend_from_slice(text.as_bytes());
        frame
    }

    fn recv_text_frame(socket: &StdUdpSocket) -> String {
        let mut buffer = [0u8; 65535];
        let (size, _) = socket.recv_from(&mut buffer).expect("receive frame");
        let parsed_size =
            u32::from_be_bytes(buffer[0..4].try_into().expect("frame header")) as usize;
        assert_eq!(
            size,
            parsed_size + 4,
            "frame should contain one full payload"
        );
        String::from_utf8(buffer[4..size].to_vec()).expect("utf8 frame")
    }

    fn test_runtime_with_installed(
        multicast: MulticastRelay,
        rejoin_interval: Duration,
    ) -> MulticastTransportRuntime {
        MulticastTransportRuntime {
            multicast: Some(multicast),
            to_send: Vec::new(),
            flush_backoff: None,
            rejoin_interval,
        }
    }

    fn queued_event(id: &str) -> EventClientMessage {
        EventClientMessage {
            note_json: format!(r#"{{"id":"{id}"}}"#),
        }
    }

    #[test]
    fn multicast_maintenance_deadline_uses_flush_backoff_before_rejoin() {
        let mut relay = test_multicast_relay();
        relay.set_rejoin_interval(Duration::from_secs(200));
        let rejoin_deadline = relay.next_rejoin_deadline();
        let mut runtime = test_runtime_with_installed(relay, Duration::from_secs(200));
        runtime.to_send.push(queued_event("flush-backoff"));
        runtime.flush_backoff = Some(FlushBackoff::new(MAX_MULTICAST_FLUSH_BACKOFF));

        let flush_deadline = runtime
            .flush_backoff
            .as_ref()
            .expect("flush backoff")
            .retry_deadline();
        let deadline = runtime
            .next_maintenance_deadline()
            .expect("maintenance deadline");

        assert_eq!(deadline, flush_deadline);
        assert!(
            deadline < rejoin_deadline,
            "flush retry should wake before multicast rejoin"
        );
    }

    #[test]
    fn multicast_maintenance_deadline_flushes_connected_queue_immediately_without_backoff() {
        let relay = test_multicast_relay();
        let mut runtime = test_runtime_with_installed(relay, Duration::from_secs(200));
        runtime.to_send.push(queued_event("immediate-flush"));

        let before = Instant::now();
        let deadline = runtime
            .next_maintenance_deadline()
            .expect("maintenance deadline");
        let after = Instant::now();

        assert!(deadline >= before);
        assert!(deadline <= after);
    }

    #[test]
    fn multicast_maintenance_deadline_ignores_flush_backoff_when_disconnected() {
        let mut relay = test_multicast_relay();
        relay.set_rejoin_interval(Duration::from_secs(200));
        relay.status = RelayStatus::Disconnected;
        let rejoin_deadline = relay.next_rejoin_deadline();
        let mut runtime = test_runtime_with_installed(relay, Duration::from_secs(200));
        runtime.to_send.push(queued_event("disconnected-flush"));
        runtime.flush_backoff = Some(FlushBackoff::new(MAX_MULTICAST_FLUSH_BACKOFF));

        assert_eq!(runtime.next_maintenance_deadline(), Some(rejoin_deadline));
    }

    /// One backend multicast work item should drain every datagram already
    /// queued on the socket.
    #[test]
    fn receive_drains_currently_available_frames() {
        let relay = test_multicast_relay();
        let local_addr = match relay.socket.local_addr().expect("relay local addr") {
            SocketAddr::V4(addr) => {
                SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, addr.port()))
            }
            SocketAddr::V6(_) => panic!("expected ipv4 relay socket"),
        };
        let sender = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind sender");
        let mut runtime = test_runtime_with_installed(relay, Duration::from_millis(20));
        let frames = [
            r#"{"id":"queued-a"}"#,
            r#"{"id":"queued-b"}"#,
            r#"{"id":"queued-c"}"#,
        ];

        for frame in frames {
            sender
                .send_to(&encode_text_frame(frame), local_addr)
                .expect("send frame");
        }
        std::thread::sleep(Duration::from_millis(20));

        let mut delivered = Vec::new();
        let received = runtime.try_recv(|event| delivered.push(event.event_json.to_owned()));

        assert_eq!(received, frames.len());
        assert_eq!(
            delivered,
            frames
                .iter()
                .map(|frame| frame.to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(runtime.try_recv(|_| panic!("socket should be drained")), 0);
    }

    /// A queued note should flush through the real runtime once a real relay is
    /// installed later.
    #[test]
    fn queued_publish_flushes_after_later_install() {
        let receiver = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind receiver");
        receiver
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("set read timeout");
        let target = match receiver.local_addr().expect("receiver addr") {
            SocketAddr::V4(addr) => addr,
            SocketAddr::V6(_) => panic!("expected ipv4 receiver"),
        };
        let relay_socket = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .expect("bind relay socket");
        let relay = MulticastRelay::new(target, relay_socket, Ipv4Addr::UNSPECIFIED);
        let queued = EventClientMessage {
            note_json: r#"{"id":"queued-note"}"#.to_owned(),
        };

        let mut runtime = MulticastTransportRuntime::default();
        runtime.broadcast(queued.clone());
        assert_eq!(runtime.to_send.len(), 1);

        runtime.multicast = Some(relay);
        runtime.try_recv(|_| panic!("queue flush should not fabricate inbound events"));

        assert_eq!(recv_text_frame(&receiver), queued.to_json());
        assert!(runtime.to_send.is_empty());
    }

    /// The real runtime should surface inbound frames both before and after a
    /// forced rejoin maintenance pass.
    #[test]
    fn receive_surfaces_frames_before_and_after_rejoin() {
        let mut relay = test_multicast_relay();
        let local_addr = match relay.socket.local_addr().expect("relay local addr") {
            SocketAddr::V4(addr) => {
                SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, addr.port()))
            }
            SocketAddr::V6(_) => panic!("expected ipv4 relay socket"),
        };
        let sender = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind sender");
        let first = r#"{"id":"before-rejoin"}"#;
        let second = r#"{"id":"after-rejoin"}"#;
        let mut delivered = Vec::new();

        relay.set_rejoin_interval(Duration::from_millis(20));
        let mut runtime = test_runtime_with_installed(relay, Duration::from_millis(20));

        sender
            .send_to(&encode_text_frame(first), local_addr)
            .expect("send first frame");
        for _ in 0..20 {
            let received = runtime.try_recv(|event| {
                assert!(matches!(event.relay_type, RelayImplType::Multicast));
                delivered.push(event.event_json.to_owned());
            });
            assert!(received <= 1);
            if delivered.len() == 1 {
                assert_eq!(received, 1);
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(delivered, vec![first.to_owned()]);

        runtime
            .multicast
            .as_mut()
            .expect("installed relay")
            .last_join = Instant::now() - Duration::from_millis(25);
        runtime.try_recv(|_| panic!("forced rejoin pass should not surface an inbound frame"));
        sender
            .send_to(&encode_text_frame(second), local_addr)
            .expect("send second frame");
        for _ in 0..20 {
            let received = runtime.try_recv(|event| delivered.push(event.event_json.to_owned()));
            assert!(received <= 1);
            if delivered.len() == 2 {
                assert_eq!(received, 1);
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        assert_eq!(delivered, vec![first.to_owned(), second.to_owned()]);
    }
}
