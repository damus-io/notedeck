//! Nothing in the sandbox guest's dependency graph builds C.
//!
//! `notedeck-agent` (crates/notedeck-agent-guest) cross-compiles to
//! aarch64- and x86_64-unknown-linux-musl with only `rustup target add` and
//! rust-lld (`.cargo/config.toml`, `scripts/build-guest-agent`). One crate
//! that compiles C, such as ring, aws-lc-sys, openssl-sys or libgit2-sys,
//! quietly turns that into "every dev machine and CI runner needs a C cross
//! toolchain for musl". This catches the dependency when it's added, on any
//! machine, rather than when someone without the toolchain next builds the
//! guest.
//!
//! A crate that builds C almost always either declares `links` (a native
//! library) or build-depends on `cc` or `cmake` to drive the compiler, so
//! the check refuses both anywhere in the guest's normal and build graph for
//! both guest targets.
//!
//! `cargo test -p bin_testing --test guest_no_c`

use std::collections::{HashMap, HashSet};
use std::process::Command;

/// The crates the guest binary is built from.
const GUEST_ROOTS: &[&str] = &["notedeck-agent-guest", "notedeck-agent-proto"];

/// The guest's architectures.
const GUEST_TARGETS: &[&str] = &["aarch64-unknown-linux-musl", "x86_64-unknown-linux-musl"];

/// Crates whose only job is to drive a C or C++ compiler.
const C_BUILDERS: &[&str] = &["cc", "cmake"];

/// A crate in the guest's graph that builds C, and how it was spotted.
struct Offender {
    package: String,
    reason: String,
}

/// `cargo metadata` resolved for `target`.
fn metadata(target: &str) -> serde_json::Value {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--filter-platform",
            target,
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo metadata runs");
    assert!(
        out.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("metadata json")
}

/// Every crate the guest builds for `target` (normal and build
/// dependencies, not dev ones) that builds C.
fn offenders(target: &str) -> Vec<Offender> {
    let meta = metadata(target);
    let packages: HashMap<&str, &serde_json::Value> = meta["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .map(|p| (p["id"].as_str().expect("id"), p))
        .collect();
    let nodes: HashMap<&str, &serde_json::Value> = meta["resolve"]["nodes"]
        .as_array()
        .expect("resolve nodes")
        .iter()
        .map(|n| (n["id"].as_str().expect("id"), n))
        .collect();

    let mut seen = HashSet::new();
    let mut queue: Vec<&str> = packages
        .iter()
        .filter(|(_, p)| GUEST_ROOTS.contains(&p["name"].as_str().unwrap_or_default()))
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(
        queue.len(),
        GUEST_ROOTS.len(),
        "both guest crates are in the workspace"
    );

    let mut found = Vec::new();
    while let Some(id) = queue.pop() {
        if !seen.insert(id) {
            continue;
        }
        let package = packages[id];
        let name = package["name"].as_str().unwrap_or_default();
        if let Some(links) = package["links"].as_str() {
            found.push(Offender {
                package: name.to_owned(),
                reason: format!("links the native library {links:?}"),
            });
        }
        if C_BUILDERS.contains(&name) {
            found.push(Offender {
                package: name.to_owned(),
                reason: "drives a C compiler".to_owned(),
            });
        }
        for dep in nodes[id]["deps"].as_array().expect("deps") {
            let built = dep["dep_kinds"]
                .as_array()
                .expect("dep_kinds")
                .iter()
                .any(|k| k["kind"].as_str() != Some("dev"));
            if built {
                queue.push(dep["pkg"].as_str().expect("pkg"));
            }
        }
    }
    found
}

#[test]
fn the_guest_builds_no_c() {
    let mut report = Vec::new();
    for target in GUEST_TARGETS {
        for offender in offenders(target) {
            report.push(format!(
                "{target}: {} {}",
                offender.package, offender.reason
            ));
        }
    }
    assert!(
        report.is_empty(),
        "the sandbox guest's graph builds C, so it no longer cross-compiles with \
         rust-lld alone. Drop the dependency or find a pure-Rust one:\n  {}",
        report.join("\n  ")
    );
}
