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

mod support;

use support::graph::{Features, Metadata};

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

/// Every crate the guest builds for `target` (normal and build
/// dependencies, not dev ones) that builds C. Default features: what a
/// plain build of the guest resolves.
fn offenders(target: &str) -> Vec<Offender> {
    let meta = Metadata::resolve(target, Features::Default);
    let graph = meta.graph();
    let roots = graph.ids_named(GUEST_ROOTS);
    assert_eq!(
        roots.len(),
        GUEST_ROOTS.len(),
        "both guest crates are in the workspace"
    );

    let mut found = Vec::new();
    for id in graph.reachable(&roots) {
        let node = &graph.nodes[id];
        if let Some(links) = node.links {
            found.push(Offender {
                package: node.name.to_owned(),
                reason: format!("links the native library {links:?}"),
            });
        }
        if C_BUILDERS.contains(&node.name) {
            found.push(Offender {
                package: node.name.to_owned(),
                reason: "drives a C compiler".to_owned(),
            });
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
    report.sort();
    assert!(
        report.is_empty(),
        "the sandbox guest's graph builds C, so it no longer cross-compiles with \
         rust-lld alone. Drop the dependency or find a pure-Rust one:\n  {}",
        report.join("\n  ")
    );
}
