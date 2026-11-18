//! What a guest boots: an OCI image, and our own `notedeck-agent` copied
//! into it before boot. There is no install step and no image of ours to
//! build or publish.
//!
//! ```text
//!   host                                   guest rootfs (image + patches)
//!   guest::locate(bundle, arch) ──copy_file──▶ /usr/local/bin/notedeck-agent  (0755)
//!   GuestSetup.image  ───────── pull once ───▶ /bin/sh, git, the toolchain
//! ```
//!
//! # Which image
//!
//! The agent is a static musl binary, so it needs nothing from the image.
//! What does is the tool surface it serves: `Bash` runs `/bin/sh`, and the
//! agent's own git calls want `git` on `PATH`. `Read`/`Write`/`Edit`/`Grep`
//! are pure Rust. So the floor is **a shell and git**, and
//! [`DEFAULT_IMAGE`] (`alpine/git`) is that floor: booted in 134 ms in the
//! spike, 3.04 s for its first-ever pull, published for both guest
//! architectures.
//!
//! Anything above the floor is the project's toolchain, and that has no
//! single answer: a session that runs `cargo build` needs Rust, which
//! `alpine/git` doesn't have. So the image is a field of the sandbox, not a
//! constant of this crate: [`GuestSetup::image`] overrides the default, and
//! the session or agent spec that asks for a sandbox is where a project's
//! image gets named. An override has two requirements the default already
//! meets:
//!
//! - **It must exist for the host's architecture.** libkrun virtualises and
//!   doesn't emulate, so an x86-64-only image won't boot on Apple Silicon.
//! - **It must have a shell and git**, or `Bash` and git calls fail in
//!   ways that look like the agent's fault.
//!
//! A first pull is paid once per image, on whichever session first asks for
//! it: 3 s for alpine, 14.8 s for `node:22-bookworm`. Pulling a large
//! toolchain image ahead of first use is left to whoever introduces one.
//!
//! # Why the binary is copied, not baked
//!
//! Copying the binary in before boot (a microsandbox rootfs patch) costs
//! nothing measurable: 147 ms to a booted guest with the 484 KB binary in
//! place, against 145 ms without it. It also makes the guest whatever this
//! notedeck was built with, so host and guest can't drift apart; the
//! handshake ([`crate::guest::HOST_BUILD`]) still checks.

use std::path::{Path, PathBuf};

use crate::guest::{self, GuestArch};
use crate::SandboxError;

/// The image a guest boots unless its sandbox names another: a shell and
/// git, the least the agent's tools need.
pub const DEFAULT_IMAGE: &str = "alpine/git";

/// Where the guest's tool executor lives inside the guest: the binary to
/// pass to `exec::start`.
pub const AGENT_PATH: &str = "/usr/local/bin/notedeck-agent";

/// The file mode the agent is copied in with: executable by everyone,
/// writable by nobody but root.
pub const AGENT_MODE: u32 = 0o755;

/// The image to boot and the agent to copy into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestSetup {
    /// An OCI reference, pulled on first use. [`DEFAULT_IMAGE`] unless
    /// overridden with [`GuestSetup::image`].
    pub image: String,
    /// The host path of the `notedeck-agent` binary to copy in.
    pub agent: PathBuf,
}

impl GuestSetup {
    /// The default image with the agent at `agent`, a host path.
    pub fn new(agent: impl Into<PathBuf>) -> Self {
        Self {
            image: DEFAULT_IMAGE.to_owned(),
            agent: agent.into(),
        }
    }

    /// The default image with this host's guest binary: the one in
    /// `bundle` if it has one, else this checkout's dev build (see
    /// [`guest::locate`]).
    pub fn for_host(bundle: Option<&Path>) -> Result<Self, SandboxError> {
        let arch = GuestArch::host().ok_or(SandboxError::UnsupportedArch)?;
        Ok(Self::new(guest::locate(bundle, arch)?))
    }

    /// Boot `image` instead of the default: a project's toolchain image.
    /// It must have a shell and git, for this host's architecture.
    pub fn image(mut self, image: impl Into<String>) -> Self {
        self.image = image.into();
        self
    }

    /// Set the image on `builder` and copy the agent in before boot (see
    /// [`GuestSetup::agent_patch`]).
    #[cfg(microvm)]
    pub fn apply(
        &self,
        builder: microsandbox::sandbox::SandboxBuilder,
    ) -> microsandbox::sandbox::SandboxBuilder {
        builder
            .image(self.image.as_str())
            .add_patch(self.agent_patch())
    }

    /// The rootfs patch that copies the agent to [`AGENT_PATH`],
    /// executable. It replaces a file the image already has there, so an
    /// image that happens to ship a `notedeck-agent` still runs ours.
    #[cfg(microvm)]
    pub fn agent_patch(&self) -> microsandbox::sandbox::Patch {
        microsandbox::sandbox::Patch::CopyFile {
            src: self.agent.clone(),
            dst: AGENT_PATH.to_owned(),
            mode: Some(AGENT_MODE),
            replace: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_a_shell_and_git() {
        let setup = GuestSetup::new("/x/notedeck-agent");
        assert_eq!(setup.image, "alpine/git");
        assert_eq!(setup.agent, Path::new("/x/notedeck-agent"));
    }

    #[test]
    fn a_project_image_overrides_the_default() {
        let setup = GuestSetup::new("/x/notedeck-agent").image("rust:1-alpine");
        assert_eq!(setup.image, "rust:1-alpine");
    }

    /// The agent goes where `PATH` finds it, so the in-guest path and the
    /// bare name both run it.
    #[test]
    fn the_agent_lands_on_path_under_its_own_name() {
        let path = Path::new(AGENT_PATH);
        assert_eq!(path.file_name().unwrap(), guest::BINARY);
        assert_eq!(path.parent().unwrap(), Path::new("/usr/local/bin"));
    }

    /// The host's guest is found the same way `guest::locate` finds it.
    #[test]
    fn for_host_takes_the_bundled_guest() {
        let Some(arch) = GuestArch::host() else {
            return;
        };
        let bundle = tempfile::tempdir().unwrap();
        let bin = guest::bundled(bundle.path(), arch);
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"\x7fELF").unwrap();

        let setup = GuestSetup::for_host(Some(bundle.path())).unwrap();
        assert_eq!(setup, GuestSetup::new(bin));
    }

    /// The patch microsandbox gets is a replacing, executable copy of the
    /// host binary to AGENT_PATH.
    #[cfg(microvm)]
    #[test]
    fn the_agent_is_copied_in_executable_and_replacing() {
        use microsandbox::sandbox::Patch;

        let setup = GuestSetup::new("/host/notedeck-agent");
        let Patch::CopyFile {
            src,
            dst,
            mode,
            replace,
        } = setup.agent_patch()
        else {
            panic!("not a file copy");
        };
        assert_eq!(src, Path::new("/host/notedeck-agent"));
        assert_eq!(dst, AGENT_PATH);
        assert_eq!(mode, Some(0o755));
        assert!(replace);
    }
}
