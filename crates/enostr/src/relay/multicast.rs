use ewebsock::{WsEvent, WsMessage};
use mio::net::UdpSocket;
use std::io;
use std::net::IpAddr;
use std::net::{SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

use crate::{EventClientMessage, RelayStatus, Result};
use std::net::Ipv4Addr;
use tracing::{debug, error};

pub struct MulticastRelay {
    last_join: Instant,
    status: RelayStatus,
    address: SocketAddrV4,
    socket: UdpSocket,
    interface: Ipv4Addr,
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
        (Instant::now() - self.last_join) >= Duration::from_secs(200)
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

        debug!("writing to multicast relay");
        let mut buf: Vec<u8> = Vec::with_capacity(4 + len);

        // Write the length of the message as 4 bytes (big-endian)
        buf.extend_from_slice(&(len as u32).to_be_bytes());

        // Append the JSON message bytes
        buf.extend_from_slice(json.as_bytes());

        self.socket.send_to(&buf, SocketAddr::V4(self.address))?;
        Ok(())
    }

    pub fn status(&self) -> RelayStatus {
        self.status
    }
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

    // wakeup our render thread when we have new stuff on the socket
    std::thread::spawn(move || {
        let mut events = Events::with_capacity(1);
        loop {
            if let Err(err) = poll.poll(&mut events, None) {
                error!("multicast socket poll error: {err}. ending multicast poller.");
                return;
            }
            wakeup();

            std::thread::yield_now();
        }
    });

    Ok(MulticastRelay::new(multicast_address, socket, interface))
}
