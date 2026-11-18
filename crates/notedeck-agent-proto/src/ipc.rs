//! The guest's way to notedeck's own socket: the `notedeck-agent` flags the
//! host starts it with, spelled once so the two ends can't drift.
//!
//! ```text
//!   guest: nd ── $NOTEDECK_SOCKET ──▶ notedeck-agent bridge ── vsock CID 2:<port> ──┐
//!   host:  notedeck's socket ◀──── microsandbox's vsock route (Unix stream) ◀───────┘
//! ```
//!
//! The guest's loopback is the guest, so a guest can't reach notedeck at
//! any address the host would use. The host routes a vsock port to
//! notedeck's Unix socket; `notedeck-agent`, started with both flags below,
//! serves a Unix socket inside the guest at the path the first one names
//! and splices each connection onto that port. `nd` in the guest then works
//! unchanged, pointed at the guest path by `$NOTEDECK_SOCKET`.

/// `notedeck-agent --ipc-socket <path>`: where in the guest to serve
/// notedeck's socket. Needs [`VSOCK_PORT_FLAG`] beside it.
pub const SOCKET_FLAG: &str = "--ipc-socket";

/// `notedeck-agent --ipc-vsock-port <port>`: the vsock port on the host
/// (CID 2) that each connection to [`SOCKET_FLAG`]'s socket is spliced onto.
pub const VSOCK_PORT_FLAG: &str = "--ipc-vsock-port";
