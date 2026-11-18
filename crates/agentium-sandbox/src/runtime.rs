//! The host-side microVM runtime: microsandbox's `msb` binary and the
//! `libkrunfw` library it boots guests with (~54 MB together).
//!
//! These are not built by cargo. microsandbox's own `prebuilt` feature
//! would download them into `~/.microsandbox` from its build script, on
//! every machine that compiles notedeck; we turn that off (Cargo.toml) and
//! get them one of two ways instead, at run time:
//!
//! - **A shipped notedeck carries them** in a bundle directory beside its
//!   executable, laid out like microsandbox's own home:
//!
//!   ```text
//!   <bundle>/bin/msb
//!   <bundle>/lib/libkrunfw.so.<ver>      (Linux; libkrunfw.<abi>.dylib on macOS)
//!   <bundle>/guest/<triple>/notedeck-agent   (see crate::guest)
//!   ```
//!
//!   Packaging fills it with [`stage`] (`cargo run -p agentium-sandbox
//!   --features microvm --example stage_runtime -- <bundle>`) and notedeck
//!   calls [`use_bundled`] at startup. The bundle is read-only;
//!   microsandbox's state (images, its database, sandbox records) still
//!   lives in its home, `~/.microsandbox` or `$MSB_HOME`.
//!
//! - **A dev checkout installs them once** into `~/.microsandbox` with
//!   [`install`], or the `msb` CLI's own installer. Nothing is downloaded
//!   unless something asks.
//!
//! [`resolve`] reports which files microsandbox will use, so a missing
//! runtime is a clear error before any sandbox is created.

use std::path::{Path, PathBuf};

use crate::SandboxError;

/// The runtime files microsandbox resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFiles {
    /// The `msb` binary; each sandbox runs as one of its processes.
    pub msb: PathBuf,
    /// The `libkrunfw` library: the guest kernel, as a shared object.
    pub libkrunfw: PathBuf,
}

/// The `msb` binary inside a bundle directory.
pub fn bundled_msb(bundle: &Path) -> PathBuf {
    bundle.join("bin").join(msb_name())
}

/// Point microsandbox at the runtime in `bundle`, if it has one, and say
/// whether it did.
///
/// Only `msb` is named: microsandbox looks for `libkrunfw` in `../lib/`
/// beside the `msb` it resolved, which is the bundle's layout. This sets a
/// process-wide default inside microsandbox (set-once; a later call is
/// ignored), below the `MSB_PATH` environment variable, so a developer can
/// still override a shipped runtime.
pub fn use_bundled(bundle: &Path) -> bool {
    let msb = bundled_msb(bundle);
    if !msb.is_file() {
        return false;
    }
    microsandbox::config::set_sdk_msb_path(msb);
    true
}

/// The runtime files microsandbox will use, or why there are none.
pub fn resolve() -> Result<RuntimeFiles, SandboxError> {
    Ok(RuntimeFiles {
        msb: microsandbox::config::resolve_msb_path()?,
        libkrunfw: microsandbox::config::resolve_libkrunfw_path()?,
    })
}

/// Download the runtime for this microsandbox version into its home
/// (`~/.microsandbox`, or `$MSB_HOME`). Skips the download when the right
/// version is already there. Network: ~54 MB from microsandbox's GitHub
/// releases.
pub async fn install() -> Result<(), SandboxError> {
    microsandbox::setup::install().await?;
    Ok(())
}

/// Download the runtime into `bundle` in the layout [`use_bundled`] reads:
/// what packaging runs to ship it.
pub async fn stage(bundle: &Path) -> Result<(), SandboxError> {
    microsandbox::setup::Setup::builder()
        .base_dir(bundle)
        .build()
        .install()
        .await?;
    Ok(())
}

/// `msb`'s file name on this platform.
fn msb_name() -> &'static str {
    if cfg!(windows) {
        "msb.exe"
    } else {
        "msb"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_bundle_is_not_used() {
        let bundle = tempfile::tempdir().unwrap();
        assert!(!use_bundled(bundle.path()));
    }

    /// Stage a real runtime into a temp bundle and check microsandbox
    /// resolves both files from it. Downloads ~54 MB, so it's opt-in:
    /// `cargo test -p agentium-sandbox --features microvm -- --ignored`.
    #[tokio::test]
    #[ignore = "downloads the microsandbox runtime"]
    async fn a_staged_bundle_resolves() {
        let bundle = tempfile::tempdir().unwrap();
        stage(bundle.path()).await.unwrap();

        assert!(use_bundled(bundle.path()));
        let files = resolve().unwrap();
        assert_eq!(files.msb, bundled_msb(bundle.path()));
        assert!(
            files.libkrunfw.starts_with(bundle.path().join("lib")),
            "{}",
            files.libkrunfw.display()
        );
    }
}
