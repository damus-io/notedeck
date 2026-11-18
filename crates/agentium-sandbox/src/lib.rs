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
//! - [`boot`]: boot a guest and run one command in it, the smoke test that
//!   the whole chain works on a machine.
//! - [`client`]: the host's end of the tool protocol, one session with the
//!   guest's executor carrying many calls. Transport-generic, so it builds
//!   (and is tested) without the microVM runner.
//! - [`exec`]: that session over microsandbox's `exec_stream`.

pub mod client;
pub mod guest;
pub mod image;
pub mod mounts;

#[cfg(microvm)]
pub mod boot;
#[cfg(microvm)]
pub mod exec;
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
