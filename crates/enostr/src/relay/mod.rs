pub mod message;
mod multicast;
pub mod pool;
pub mod subs_debug;
mod websocket;

pub use websocket::{WebsocketConn, WebsocketRelay};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum RelayStatus {
    Connected,
    Connecting,
    Disconnected,
}
