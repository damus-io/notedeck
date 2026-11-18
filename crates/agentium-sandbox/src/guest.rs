//! The guest side: which `notedeck-agent` binary a sandbox boots with, and
//! the build stamp the host holds it to.
//!
//! `scripts/build-guest-agent` cross-compiles `notedeck-agent` (crate
//! `notedeck-agent-guest`) for both guest architectures. A dev checkout
//! finds it in its own target dir; a shipped notedeck carries it in its
//! bundle directory (see [`crate::runtime`] for the layout), next to the
//! microsandbox runtime. The binary is copied into the guest before boot,
//! so there is no image to build and no install step.

use std::path::{Path, PathBuf};

use crate::SandboxError;

/// The name of the guest's tool executor binary.
pub const BINARY: &str = "notedeck-agent";

/// This build's stamp: the commit it was built from (`-dirty` when the tree
/// had uncommitted changes), or whatever `NOTEDECK_AGENT_BUILD` said at
/// build time. `scripts/build-guest-agent` stamps the guest by the same
/// rule, so a host and a guest built from one checkout agree, and a host
/// handed some other build's guest can say so instead of misbehaving.
pub const HOST_BUILD: &str = env!("NOTEDECK_AGENT_BUILD");

/// A guest CPU architecture. libkrun virtualises rather than emulates, so a
/// guest always runs the host's architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestArch {
    /// arm64: Apple-Silicon macOS and arm64 Linux hosts.
    Aarch64,
    /// x86-64: most KVM Linux hosts.
    X86_64,
}

impl GuestArch {
    /// The guest architecture for this host, if it has one.
    pub const fn host() -> Option<Self> {
        if cfg!(target_arch = "aarch64") {
            Some(Self::Aarch64)
        } else if cfg!(target_arch = "x86_64") {
            Some(Self::X86_64)
        } else {
            None
        }
    }

    /// The rust target the guest binary is built for.
    pub const fn triple(self) -> &'static str {
        match self {
            Self::Aarch64 => "aarch64-unknown-linux-musl",
            Self::X86_64 => "x86_64-unknown-linux-musl",
        }
    }
}

/// Where a bundle directory keeps the guest binary for `arch`:
/// `<bundle>/guest/<triple>/notedeck-agent`.
pub fn bundled(bundle: &Path, arch: GuestArch) -> PathBuf {
    bundle.join("guest").join(arch.triple()).join(BINARY)
}

/// Where `scripts/build-guest-agent` leaves the guest binary for `arch` in
/// the target dir this crate was built in:
/// `<target>/<triple>/small/notedeck-agent`. Only meaningful on the machine
/// that built notedeck.
pub fn dev_build(arch: GuestArch) -> Option<PathBuf> {
    let target = option_env!("AGENTIUM_SANDBOX_TARGET_DIR")?;
    Some(
        Path::new(target)
            .join(arch.triple())
            .join("small")
            .join(BINARY),
    )
}

/// The guest binary to boot for `arch`: the bundle's if `bundle` has one,
/// else this checkout's dev build.
pub fn locate(bundle: Option<&Path>, arch: GuestArch) -> Result<PathBuf, SandboxError> {
    let candidates = bundle
        .map(|b| bundled(b, arch))
        .into_iter()
        .chain(dev_build(arch));

    let mut searched = String::new();
    for path in candidates {
        if path.is_file() {
            return Ok(path);
        }
        if !searched.is_empty() {
            searched.push(':');
        }
        searched.push_str(&path.to_string_lossy());
    }
    Err(SandboxError::GuestMissing {
        triple: arch.triple(),
        searched,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_wins_over_the_dev_build() {
        let bundle = tempfile::tempdir().unwrap();
        let path = bundled(bundle.path(), GuestArch::X86_64);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"\x7fELF").unwrap();

        let found = locate(Some(bundle.path()), GuestArch::X86_64).unwrap();
        assert_eq!(found, path);
    }

    #[test]
    fn a_missing_guest_names_every_place_it_looked() {
        let bundle = tempfile::tempdir().unwrap();
        // aarch64 isn't built by a plain test run, so the dev build is
        // missing too unless someone ran the script; skip then.
        let dev = dev_build(GuestArch::Aarch64).expect("build.rs found the target dir");
        if dev.is_file() {
            return;
        }

        let err = locate(Some(bundle.path()), GuestArch::Aarch64).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("aarch64-unknown-linux-musl"), "{msg}");
        assert!(msg.contains(&*bundle.path().to_string_lossy()), "{msg}");
        assert!(msg.contains(&*dev.to_string_lossy()), "{msg}");
        assert!(msg.contains("scripts/build-guest-agent"), "{msg}");
    }

    /// The dev path is the one scripts/build-guest-agent prints.
    #[test]
    fn the_dev_build_is_under_the_workspace_target_dir() {
        let dev = dev_build(GuestArch::X86_64).unwrap();
        assert!(
            dev.ends_with("x86_64-unknown-linux-musl/small/notedeck-agent"),
            "{}",
            dev.display()
        );
        let target = dev.ancestors().nth(3).unwrap();
        assert!(
            target.join("debug").is_dir() || target.join("release").is_dir(),
            "{} isn't a cargo target dir",
            target.display()
        );
    }

    #[test]
    fn the_stamp_is_a_commit_or_an_override() {
        assert!(!HOST_BUILD.is_empty());
        if std::env::var_os("NOTEDECK_AGENT_BUILD").is_none() && HOST_BUILD != "unknown" {
            let sha = HOST_BUILD.trim_end_matches("-dirty");
            assert_eq!(sha.len(), 40, "{HOST_BUILD}");
            assert!(sha.bytes().all(|b| b.is_ascii_hexdigit()), "{HOST_BUILD}");
        }
    }
}
