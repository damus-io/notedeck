mod identity;
mod limits;
pub mod message;
mod multicast;
pub mod pool;
pub mod subs_debug;
mod websocket;

pub use identity::{
    NormRelayUrl, OutboxSubId, RelayId, RelayReqId, RelayReqStatus, RelayType, RelayUrlPkgs,
};
pub use limits::{
    RelayCoordinatorLimits, RelayLimitations, SubPass, SubPassGuardian, SubPassRevocation,
};
pub use websocket::{WebsocketConn, WebsocketRelay};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum RelayStatus {
    Connected,
    Connecting,
    Disconnected,
}
