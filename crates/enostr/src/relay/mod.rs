use ewebsock::{Options, WsEvent, WsMessage, WsReceiver, WsSender};
use mio::net::UdpSocket;
use std::io;
use std::net::IpAddr;
use std::net::{SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

use crate::{ClientMessage, EventClientMessage, Result};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::net::Ipv4Addr;
use tracing::{debug, error};

pub mod message;
pub mod pool;

#[derive(Debug, Copy, Clone)]
pub enum RelayStatus {
    Connected,
    Connecting,
    Disconnected,
}

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
        let json = msg.to_json()?;
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
            if let Err(err) = poll.poll(&mut events, Some(Duration::from_millis(100))) {
                error!("multicast socket poll error: {err}. ending multicast poller.");
                return;
            }
            wakeup();

            std::thread::yield_now();
        }
    });

    Ok(MulticastRelay::new(multicast_address, socket, interface))
}

pub struct Relay {
    pub url: String,
    pub status: RelayStatus,
    pub sender: WsSender,
    pub receiver: WsReceiver,
}

impl fmt::Debug for Relay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Relay")
            .field("url", &self.url)
            .field("status", &self.status)
            .finish()
    }
}

impl Hash for Relay {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Hashes the Relay by hashing the URL
        self.url.hash(state);
    }
}

impl PartialEq for Relay {
    fn eq(&self, other: &Self) -> bool {
        self.url == other.url
    }
}

impl Eq for Relay {}

impl Relay {
    pub fn new(url: String, wakeup: impl Fn() + Send + Sync + 'static) -> Result<Self> {
        let status = RelayStatus::Connecting;
        let (sender, receiver) = ewebsock::connect_with_wakeup(&url, Options::default(), wakeup)?;

        Ok(Self {
            url,
            sender,
            receiver,
            status,
        })
    }

    pub fn send(&mut self, msg: &ClientMessage) {
        let json = match msg.to_json() {
            Ok(json) => {
                debug!("sending {} to {}", json, self.url);
                json
            }
            Err(e) => {
                error!("error serializing json for filter: {e}");
                return;
            }
        };

        let txt = WsMessage::Text(json);
        self.sender.send(txt);
    }

    pub fn connect(&mut self, wakeup: impl Fn() + Send + Sync + 'static) -> Result<()> {
        let (sender, receiver) =
            ewebsock::connect_with_wakeup(&self.url, Options::default(), wakeup)?;
        self.status = RelayStatus::Connecting;
        self.sender = sender;
        self.receiver = receiver;
        Ok(())
    }

    pub fn ping(&mut self) {
        let msg = WsMessage::Ping(vec![]);
        self.sender.send(msg);
    }
}
