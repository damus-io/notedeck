pub mod message;
mod multicast;
pub mod pool;
pub mod subs_debug;
mod websocket;

pub use websocket::WebsocketConn;

#[derive(Debug, Copy, Clone)]
pub enum RelayStatus {
    Connected,
    Connecting,
    Disconnected,
}
