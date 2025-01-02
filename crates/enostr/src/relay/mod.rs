use ewebsock::{Options, WsEvent, WsMessage, WsReceiver, WsSender};
use std::io;
use std::net::UdpSocket;

use crate::{ClientMessage, Result};
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
    address: String,
    receiver: UdpReceiver,
}

impl MulticastRelay {
    pub fn new(address: String, receiver: UdpReceiver) -> Self {
        MulticastRelay { address, receiver }
    }

    pub fn send(&self, msg: &ClientMessage) -> Result<()> {
        let json = msg.to_json()?;
        let len = json.len();

        debug!("writing to multicast relay");
        let mut buf: Vec<u8> = Vec::with_capacity(4 + len);

        // Write the length of the message as 4 bytes (big-endian)
        buf.extend_from_slice(&(len as u32).to_be_bytes());

        // Append the JSON message bytes
        buf.extend_from_slice(json.as_bytes());

        self.receiver.socket.send_to(&buf, &self.address)?;
        Ok(())
    }
}

pub fn setup_multicast_relay() -> Result<MulticastRelay> {
    let address = "239.19.88.1:9797".to_string();
    let multicast_ip = Ipv4Addr::new(239, 19, 88, 1);

    let socket = UdpSocket::bind("0.0.0.0:9797")?;
    let interface = Ipv4Addr::new(192, 168, 100, 161);

    socket.join_multicast_v4(&multicast_ip, &interface)?;
    socket.set_nonblocking(true)?;

    Ok(MulticastRelay::new(address, UdpReceiver::new(socket)))
}

pub struct UdpReceiver {
    socket: UdpSocket,
}

impl UdpReceiver {
    pub fn new(socket: UdpSocket) -> Self {
        Self { socket }
    }

    pub fn try_recv(&self) -> Option<WsEvent> {
        let mut buffer = [0u8; 65535];
        // Read the size header
        match self.socket.recv_from(&mut buffer) {
            Ok((size, src)) => {
                let parsed_size = u32::from_be_bytes(buffer[0..4].try_into().ok()?) as usize;
                debug!("multicast: read size {} from start of header", size - 4);

                if size != parsed_size {
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
