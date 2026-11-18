//! Letting a guest reach notedeck, so `nd` (headway, agentium, notebook)
//! works inside it.
//!
//! ```text
//!   guest                                                   host
//!   nd ─▶ $NOTEDECK_SOCKET = GUEST_SOCKET                    notedeck's socket
//!          │  (served by notedeck-agent's ipc bridge)          ▲
//!          └─▶ vsock CID 2 : VSOCK_PORT ══ microsandbox route ═╝  (a Unix stream)
//! ```
//!
//! # Why vsock and not the network
//!
//! The guest's `127.0.0.1` is the guest. The spike for this
//! (headway:dave/aim-clown-announce) measured every other way in: the
//! sandbox gateway is microsandbox's own network stack, not a proxy to the
//! host's loopback, and `.port()` publishes the other direction. Only the
//! host's LAN address answered, which would mean notedeck listening on the
//! LAN, an address DHCP moves, and an egress rule ([`crate::egress`]) to
//! the host. microsandbox's vsock route instead joins a guest port to a
//! host Unix socket without touching any network, so it needs no egress
//! rule and works with no network at all. And since `nd` already talks to
//! notedeck over a Unix socket, the route's host end is that socket itself:
//! nothing on the host proxies.
//!
//! In the guest, `notedeck-agent` started with [`NotedeckIpc::agent_args`]
//! serves a Unix socket at [`GUEST_SOCKET`] and splices each connection
//! onto the vsock port (`notedeck_agent_guest::ipc_bridge`), and the
//! sandbox's environment ([`NotedeckIpc::guest_env`]) points `nd` at it.
//! The bridge lives as long as the executor, which is as long as the
//! session's tool calls can run `nd`.
//!
//! # What this hands the guest: opt-in, never a default
//!
//! **notedeck's socket is every notedeck tool, and those tools act as the
//! user**: they move and comment on cards and publish under the account
//! notedeck runs as. Without this route a guest holds no key and has no way
//! to reach anything that does, so it structurally cannot act as the user
//! (headway:dave/normal-search-order). With it, it can, through any tool
//! the socket serves.
//!
//! So nothing applies a [`NotedeckIpc`] for you: a sandbox reaches notedeck
//! only when whoever builds it calls [`NotedeckIpc::apply`] and starts the
//! executor with it. Do that for an agent you'd let run `nd` on the host,
//! not for one that is sandboxed because it isn't trusted. Narrowing the
//! route to some tools would mean pointing it at a filtering socket instead
//! of notedeck's own; [`NotedeckIpc::new`] takes any host socket, so that
//! needs no change here.
//!
//! # When notedeck isn't there
//!
//! The route is dialled per connection, so a sandbox boots fine without a
//! running notedeck. `nd` in the guest then gets no answer ("notedeck hung
//! up without answering", or a connection reset) and exits 1 rather than 2,
//! because the bridge accepts before it can know. `ND_NO_START` keeps `nd`
//! from trying to start a notedeck of its own in the guest.

use std::path::{Path, PathBuf};

use notedeck_agent_proto::ipc as flags;

use crate::mounts::GuestEnv;
use crate::SandboxError;

/// Where `nd` in the guest finds notedeck: the socket the executor's bridge
/// serves. Under `/run`, which is the guest's own, so it can't land in a
/// mounted checkout.
pub const GUEST_SOCKET: &str = "/run/notedeck/notedeck.sock";

/// The vsock port on the host (CID 2) routed to notedeck's socket. Any
/// port no other route uses; `0x6e64` is "nd" in ASCII.
pub const VSOCK_PORT: u32 = 0x6e64;

/// `nd`'s switch for "don't start a notedeck when none answers"
/// (`ND_NO_START` in crates/nd_cli/src/start.rs, a binary crate).
pub const NO_START_ENV: &str = "ND_NO_START";

/// A sandbox's route to notedeck's socket on the host. Opt-in: see the
/// module docs for what it hands the guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotedeckIpc {
    host_socket: PathBuf,
}

impl NotedeckIpc {
    /// Route the guest to the notedeck socket at `host_socket`, which must
    /// be absolute. It needn't exist yet: it's dialled per connection.
    pub fn new(host_socket: impl Into<PathBuf>) -> Result<Self, SandboxError> {
        let host_socket = host_socket.into();
        if !host_socket.is_absolute() {
            return Err(SandboxError::IpcSocketNotAbsolute(host_socket));
        }
        Ok(Self { host_socket })
    }

    /// Route the guest to the socket `nd` on this host would use:
    /// `$NOTEDECK_SOCKET`, else the per-user one
    /// (`notedeck_ipc::socket_path`).
    pub fn this_host() -> Result<Self, SandboxError> {
        Self::new(notedeck_ipc::socket_path())
    }

    /// The host socket the route ends at.
    pub fn host_socket(&self) -> &Path {
        &self.host_socket
    }

    /// The environment that points `nd` in the guest at the bridge, and
    /// keeps it from starting a notedeck there.
    pub fn guest_env(&self) -> Vec<GuestEnv> {
        vec![
            GuestEnv {
                key: notedeck_ipc::SOCKET_ENV,
                value: GUEST_SOCKET.to_owned(),
            },
            GuestEnv {
                key: NO_START_ENV,
                value: "1".to_owned(),
            },
        ]
    }

    /// The `notedeck-agent` arguments that start its bridge: serve
    /// [`GUEST_SOCKET`], spliced onto [`VSOCK_PORT`].
    pub fn agent_args(&self) -> Vec<String> {
        vec![
            flags::SOCKET_FLAG.to_owned(),
            GUEST_SOCKET.to_owned(),
            flags::VSOCK_PORT_FLAG.to_owned(),
            VSOCK_PORT.to_string(),
        ]
    }

    /// Route [`VSOCK_PORT`] to the host socket and set [`Self::guest_env`]
    /// on `builder`. The executor still has to be started with
    /// [`Self::agent_args`] (`crate::exec::start` does, given this route).
    #[cfg(microvm)]
    pub fn apply(
        &self,
        builder: microsandbox::sandbox::SandboxBuilder,
    ) -> microsandbox::sandbox::SandboxBuilder {
        self.guest_env()
            .into_iter()
            .fold(builder.vsock(&self.host_socket, VSOCK_PORT), |b, e| {
                b.env(e.key, e.value)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_host_socket_is_refused() {
        let err = NotedeckIpc::new("notedeck.sock").unwrap_err();
        assert!(
            matches!(err, SandboxError::IpcSocketNotAbsolute(_)),
            "{err}"
        );
        assert!(err.to_string().contains("notedeck.sock"), "{err}");
    }

    #[test]
    fn this_host_is_the_socket_nd_would_use() {
        let ipc = NotedeckIpc::this_host().unwrap();
        assert_eq!(ipc.host_socket(), notedeck_ipc::socket_path());
    }

    /// `nd` reads `$NOTEDECK_SOCKET` (notedeck_ipc) and `$ND_NO_START`
    /// (nd_cli); the guest's env must spell both the way they do.
    #[test]
    fn the_guest_env_points_nd_at_the_bridge_and_never_starts_one() {
        let ipc = NotedeckIpc::new("/run/user/1000/notedeck/notedeck.sock").unwrap();
        let env = ipc.guest_env();
        let get = |key: &str| env.iter().find(|e| e.key == key).map(|e| e.value.as_str());
        assert_eq!(get("NOTEDECK_SOCKET"), Some(GUEST_SOCKET));
        assert_eq!(get("ND_NO_START"), Some("1"));
    }

    /// The executor is told the same socket the environment names, and the
    /// port the route carries.
    #[test]
    fn the_agent_serves_the_socket_the_env_names_on_the_routed_port() {
        let ipc = NotedeckIpc::new("/tmp/n.sock").unwrap();
        assert_eq!(
            ipc.agent_args(),
            [
                "--ipc-socket",
                GUEST_SOCKET,
                "--ipc-vsock-port",
                &VSOCK_PORT.to_string(),
            ]
        );
    }

    /// The guest socket is the guest's own, never under a path a mount
    /// could put the host's files at.
    #[test]
    fn the_guest_socket_is_under_run() {
        assert!(Path::new(GUEST_SOCKET).starts_with("/run"));
    }

    /// The built config carries the route, as a stream, to the host
    /// socket, and the env every command in the guest gets.
    #[cfg(microvm)]
    #[tokio::test]
    async fn apply_routes_the_port_to_the_host_socket_and_sets_the_env() {
        let host = "/tmp/notedeck-test.sock";
        let ipc = NotedeckIpc::new(host).unwrap();
        let config = ipc
            .apply(microsandbox::Sandbox::builder("ipc-route-test").image("alpine"))
            .build()
            .await
            .expect("the route validates");

        let routes = &config.spec.vsock.routes;
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].port, VSOCK_PORT);
        assert_eq!(routes[0].host_socket, Path::new(host));
        assert_eq!(
            routes[0].socket_type,
            microsandbox::sandbox::VsockSocketType::Stream
        );
        let env = |key: &str| {
            config
                .spec
                .env
                .iter()
                .find(|e| e.key == key)
                .map(|e| e.value.clone())
        };
        assert_eq!(env("NOTEDECK_SOCKET").as_deref(), Some(GUEST_SOCKET));
        assert_eq!(env("ND_NO_START").as_deref(), Some("1"));
    }
}
