//! Build script for notedeck_dave. One job: set `cfg(sandbox)` when the
//! `sandbox` feature is on AND the target is one microsandbox runs guests
//! on (KVM Linux, Apple-Silicon macOS), so `src/sandbox.rs` has one switch
//! to test instead of repeating the platform list beside the feature.
//!
//! The list must match agentium-sandbox's own (its build.rs and the target
//! gate in its Cargo.toml): `agentium_sandbox::guests` exists exactly when
//! this cfg is set.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(sandbox)");
    if std::env::var_os("CARGO_FEATURE_SANDBOX").is_some() && sandbox_platform() {
        println!("cargo::rustc-cfg=sandbox");
    }
}

/// The platforms microsandbox runs guests on.
fn sandbox_platform() -> bool {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    os == "linux" || (os == "macos" && arch == "aarch64")
}
