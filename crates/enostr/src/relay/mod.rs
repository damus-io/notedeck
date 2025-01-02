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

pub fn setup_multicast_socket() -> Result<UdpSocket> {
    let socket = UdpSocket::bind("0.0.0.0:9797")?;

    // Join the multicast group
    let multicast_ip = Ipv4Addr::new(239, 1, 1, 1);
    let interface = Ipv4Addr::new(0, 0, 0, 0);
    socket.join_multicast_v4(&multicast_ip, &interface)?;

    Ok(socket)
}

pub struct UdpReceiver {
    socket: UdpSocket,
}

impl UdpReceiver {
    pub fn new() -> Result<Self> {
        let socket = setup_multicast_socket()?;
        Ok(Self { socket })
    }

    pub fn try_recv(&self) -> Option<WsEvent> {
        let mut size_buffer = [0u8; 4];
        // Read the size header
        match self.socket.recv_from(&mut size_buffer) {
            Ok((4, src)) => {
                let size = u32::from_be_bytes(size_buffer) as usize;

                // Allocate buffer of exact size for the payload
                let mut buffer = vec![0u8; size];
                match self.socket.recv_from(&mut buffer) {
                    Ok((len, _)) if len == size => {
                        let text = String::from_utf8_lossy(&buffer);
                        debug!("multicast: received {} bytes from {}: {}", len, src, &text);
                        Some(WsEvent::Message(WsMessage::Text(text.to_string())))
                    }
                    Ok((len, _)) => {
                        error!(
                            "multicast: partial data received: expected {}, got {}",
                            size, len
                        );
                        None
                    }
                    Err(e) => {
                        error!("multicast: error receiving data: {}", e);
                        None
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                // No data available, continue
                None
            }
            Err(e) => {
                error!("multicast: error receiving size header: {}", e);
                None
            }
            Ok((size, _)) => {
                error!("multicast: header size wrong? {size}");
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
