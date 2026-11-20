//! Desktop-only microVM sandboxes for agents notedeck doesn't fully trust:
//! maintenance bots and sessions spawned by untrusted users. Our own coder
//! runs on the host like Claude Code does; this is the other runner.
//!
//! The guest is a [microsandbox](https://github.com/superradcompany/microsandbox)
//! microVM (libkrun) running `notedeck-agent`, the static musl tool
//! executor from `notedeck-agent-guest`. The model conversation and its
//! credential stay on the host.
//!
//! This crate exists to keep microsandbox out of every build that can't use
//! it. It is desktop-only (KVM Linux and Apple-Silicon macOS), and the
//! microVM half is behind the `microvm` feature; see Cargo.toml for the
//! three fences and `crates/bin_testing/tests/sandbox_desktop_only.rs` for
//! the check. Without the feature, or on another platform, the crate still
//! builds and [`availability`] says why there are no sandboxes.
//!
//! - [`runtime`]: where the host-side runtime (`msb` + `libkrunfw`) comes
//!   from, in a shipped build and in a dev checkout.
//! - [`guest`]: where the guest's `notedeck-agent` binary comes from, and
//!   the build stamp the host compares it against.
//! - [`image`]: what a guest boots: an OCI image (`alpine/git` unless the
//!   sandbox names a project's own) with that binary copied in before boot.
//! - [`mounts`]: which host directories a guest gets (a checkout and the
//!   git dir behind it, at the same paths as on the host), and whether the
//!   shared git state is readonly.
//! - [`egress`]: where a guest may connect and what it may resolve: a
//!   short hostname allowlist on HTTPS, everything else denied, and the
//!   boot check that DNS works under it.
//! - [`ipc`]: the opt-in route from a guest to notedeck's own socket over
//!   vsock, so `nd` works inside it.
//! - [`boot`]: boot a guest and run one command in it, the smoke test that
//!   the whole chain works on a machine.
//! - [`client`]: the host's end of the tool protocol, one session with the
//!   guest's executor carrying many calls. Transport-generic, so it builds
//!   (and is tested) without the microVM runner.
//! - [`exec`]: that session over microsandbox's `exec_stream`.
//! - [`lifecycle`]: how a guest is named and labelled, when one may be
//!   reaped, and the bound on how many run at once.
//! - [`fleet`]: the one owner of guest lifecycles: boot a session's
//!   guest in the order that works, stop and remove it, and sweep up what
//!   a crash left behind, over a [`fleet::VmHost`] so it is tested
//!   without KVM.
//! - [`guests`]: microsandbox as that host, and the `Guests` notedeck runs.

pub mod client;
pub mod egress;
pub mod fleet;
pub mod guest;
pub mod image;
pub mod ipc;
pub mod lifecycle;
pub mod mounts;

#[cfg(microvm)]
pub mod boot;
#[cfg(microvm)]
pub mod exec;
#[cfg(microvm)]
pub mod guests;
#[cfg(microvm)]
pub mod runtime;

/// Whether this build of notedeck can run microVM sandboxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// Built with the microVM runner. The host still needs the runtime
    /// ([`runtime`]) and hardware virtualisation to boot one.
    Built,
    /// The platform supports microVMs, but this build left the `microvm`
    /// feature off.
    FeatureOff,
    /// microsandbox doesn't run here: anything but KVM Linux and
    /// Apple-Silicon macOS, which includes android and iOS.
    UnsupportedPlatform,
}

/// Whether this build can run microVM sandboxes, and if not, why.
pub const fn availability() -> Availability {
    if cfg!(microvm) {
        Availability::Built
    } else if cfg!(any(
        target_os = "linux",
        all(target_os = "macos", target_arch = "aarch64")
    )) {
        Availability::FeatureOff
    } else {
        Availability::UnsupportedPlatform
    }
}

/// What can go wrong setting up or booting a sandbox.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// The guest's `notedeck-agent` binary isn't anywhere we look.
    #[error(
        "no notedeck-agent guest binary for {triple}; run scripts/build-guest-agent \
         (looked in {searched})"
    )]
    GuestMissing {
        /// The guest architecture we needed a binary for.
        triple: &'static str,
        /// Every path tried, `:`-separated.
        searched: String,
    },
    /// The host's architecture has no guest build: libkrun virtualises
    /// rather than emulates, so the guest must match it.
    #[error("no guest architecture for this host")]
    UnsupportedArch,
    /// An egress allowlist was handed something other than a bare
    /// hostname: a wildcard, a port, a URL.
    #[error("not a bare hostname for the egress allowlist: {0:?}")]
    BadHost(String),
    /// A route to notedeck's socket ([`ipc::NotedeckIpc`]) was given a
    /// relative host path; microsandbox needs an absolute one.
    #[error("the host notedeck socket must be an absolute path: {}", .0.display())]
    IpcSocketNotAbsolute(std::path::PathBuf),
    /// A dave session id that isn't a valid sandbox name as it stands
    /// (see [`lifecycle::guest_name`]).
    #[error("session id {0:?} can't name a guest: letters, digits, '.', '-', '_' only, at most 64 bytes")]
    BadSessionId(String),
    /// This notedeck already runs as many guests as it may.
    #[error("{max} sandboxed sessions are already running; stop one to start another")]
    AtCapacity {
        /// The bound.
        max: usize,
    },
    /// The session already has a guest that is up, in this notedeck or
    /// another one.
    #[error("session {0} already has a running sandbox")]
    GuestRunning(String),
    /// A sandbox that isn't ours has the name this session's guest needs.
    #[error("a sandbox named {0} exists and wasn't made by notedeck; remove or rename it")]
    GuestNameTaken(String),
    /// The guest couldn't resolve a name its egress allowlist permits: DNS
    /// is blocked by the policy or the host's resolver is unreachable.
    #[error(
        "the guest can't resolve {host} ({detail}); a sandbox resolves only the \
         names on its egress allowlist, so if {host} is on it the host's DNS is \
         unreachable from the guest"
    )]
    DnsUnresolved {
        /// The name it tried.
        host: String,
        /// What the lookup said, or that it timed out.
        detail: String,
    },
    /// microsandbox refused the egress policy we built.
    #[cfg(microvm)]
    #[error("egress policy: {0}")]
    Policy(microsandbox_network::policy::BuildError),
    /// The guest's executor wouldn't open a session: the wrong build, or it
    /// never answered.
    #[error(transparent)]
    Connect(#[from] client::ConnectError),
    /// An exec session came back without the stdin pipe it was asked for.
    #[cfg(microvm)]
    #[error("the guest exec session has no stdin")]
    NoStdin,
    /// A command in the guest exited non-zero.
    #[cfg(microvm)]
    #[error("guest command exited {code}: {stderr}")]
    GuestCommand {
        /// Its exit code.
        code: i32,
        /// What it printed to stderr.
        stderr: String,
    },
    /// microsandbox failed: no runtime installed, no hardware
    /// virtualisation, an image pull, a boot, an exec.
    #[cfg(microvm)]
    #[error("microsandbox: {0}")]
    Microsandbox(#[from] microsandbox::MicrosandboxError),
    /// A call the test host (`fleet`'s `FakeVms`) was told to fail.
    #[cfg(any(test, feature = "test-support"))]
    #[error("fake vm host: {0}")]
    Fake(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// android and iOS must never report microVMs, whatever the features.
    #[test]
    fn availability_matches_the_build() {
        let a = availability();
        if cfg!(any(target_os = "android", target_os = "ios")) {
            assert_eq!(a, Availability::UnsupportedPlatform);
        }
        if cfg!(any(
            target_os = "linux",
            all(target_os = "macos", target_arch = "aarch64")
        )) {
            assert_eq!(a == Availability::Built, cfg!(feature = "microvm"));
        }
    }
}
