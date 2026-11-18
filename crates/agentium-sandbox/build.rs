//! Build script for agentium-sandbox. Three jobs, none of which touch the
//! network or anything outside OUT_DIR:
//!
//! - `cfg(microvm)`: set when the `microvm` feature is on AND the target is
//!   one microsandbox runs on, so the code has one switch to test instead of
//!   repeating the platform list beside every feature check.
//! - `NOTEDECK_AGENT_BUILD`: this checkout's commit, the same stamp
//!   `scripts/build-guest-agent` bakes into the guest binary, so the host
//!   can refuse a guest that isn't its own build.
//! - `AGENTIUM_SANDBOX_TARGET_DIR`: the cargo target dir this crate is built
//!   into, where `scripts/build-guest-agent` leaves the guest binaries.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(microvm)");
    if std::env::var_os("CARGO_FEATURE_MICROVM").is_some() && microvm_platform() {
        println!("cargo::rustc-cfg=microvm");
    }

    println!("cargo::rustc-env=NOTEDECK_AGENT_BUILD={}", build_stamp());

    if let Some(dir) = target_dir() {
        println!(
            "cargo::rustc-env=AGENTIUM_SANDBOX_TARGET_DIR={}",
            dir.display()
        );
    }
}

/// The platforms microsandbox runs guests on: KVM Linux and Apple-Silicon
/// macOS. Must match the target gate on the dependency in Cargo.toml.
fn microvm_platform() -> bool {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    os == "linux" || (os == "macos" && arch == "aarch64")
}

/// The build stamp, by the same rule as `scripts/build-guest-agent`: an
/// explicit `NOTEDECK_AGENT_BUILD` wins (packaging sets one value for both
/// sides), else `git rev-parse HEAD` with `-dirty` for uncommitted changes,
/// else `unknown` (a source tarball).
///
/// Reruns when HEAD moves. It can't see an edit to the working tree, so a
/// dirty stamp can go stale; both sides say `<sha>-dirty` then, and the
/// check passes as it would have anyway.
fn build_stamp() -> String {
    println!("cargo::rerun-if-env-changed=NOTEDECK_AGENT_BUILD");
    if let Ok(stamp) = std::env::var("NOTEDECK_AGENT_BUILD") {
        return stamp;
    }

    let Some(sha) = git(&["rev-parse", "HEAD"]) else {
        return "unknown".to_owned();
    };
    rerun_on_head_move();

    let clean = Command::new("git")
        .args(["diff", "--quiet", "HEAD", "--"])
        .status()
        .is_ok_and(|s| s.success());
    if clean {
        sha
    } else {
        format!("{sha}-dirty")
    }
}

/// Ask cargo to rerun this script when HEAD or the branch it names moves.
/// Only files that exist are named: cargo treats a missing one as changed
/// and would rebuild the crate every time.
fn rerun_on_head_move() {
    let mut watched = vec!["HEAD".to_owned(), "packed-refs".to_owned()];
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watched.push(branch);
    }
    for name in watched {
        let Some(path) = git(&["rev-parse", "--git-path", &name]) else {
            continue;
        };
        if Path::new(&path).exists() {
            println!("cargo::rerun-if-changed={path}");
        }
    }
}

/// One line of git's output, or None if git isn't there or fails.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!line.is_empty()).then_some(line)
}

/// The target dir, from OUT_DIR's shape:
/// `<target>[/<triple>]/<profile>/build/<pkg>-<hash>/out`.
fn target_dir() -> Option<PathBuf> {
    let out = PathBuf::from(std::env::var_os("OUT_DIR")?);
    let above_profile = out.ancestors().nth(4)?;
    let triple = std::env::var("TARGET").unwrap_or_default();
    if above_profile
        .file_name()
        .is_some_and(|n| n == triple.as_str())
    {
        return above_profile.parent().map(Path::to_path_buf);
    }
    Some(above_profile.to_path_buf())
}
