//! microsandbox never reaches a build that can't use it.
//!
//! microsandbox runs guests under libkrun, which needs KVM Linux or
//! Apple-Silicon macOS, and its graph is heavy (sea-orm, sqlx, reqwest,
//! rustls, a system libcap-ng on Linux). So it lives in
//! `crates/agentium-sandbox` alone, target-gated to the desktop platforms
//! and optional behind that crate's `microvm` feature. This checks the
//! fences hold, with every feature in the workspace turned on:
//!
//! - no android or iOS build resolves any microsandbox crate;
//! - on desktop, agentium-sandbox is the only workspace crate that depends
//!   on one directly;
//! - agentium-core (meant to drop into an iOS process) never reaches
//!   agentium-sandbox;
//! - a desktop build with default features (notedeck_chrome without its
//!   `sandbox` feature) compiles no microsandbox: Dave's guests are opt-in;
//! - microsandbox's own `prebuilt` feature stays off: it makes microsandbox's
//!   build script download ~54 MB into `~/.microsandbox` on every machine
//!   that compiles it.
//!
//! `cargo test -p bin_testing --test sandbox_desktop_only`

mod support;

use support::graph::{Features, Metadata};

/// Targets that must never see microsandbox.
const MOBILE_TARGETS: &[&str] = &["aarch64-linux-android", "aarch64-apple-ios"];

/// Targets agentium-sandbox's microVM runner is built for.
const DESKTOP_TARGETS: &[&str] = &["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"];

/// The one workspace crate allowed to depend on microsandbox.
const SANDBOX_CRATE: &str = "agentium-sandbox";

/// Crates that must stay iOS-safe, so must never reach the sandbox crate.
const IOS_SAFE: &[&str] = &["agentium-core"];

/// Whether `name` is part of microsandbox or its VMM.
fn is_microsandbox(name: &str) -> bool {
    name == "microsandbox" || name.starts_with("microsandbox-") || name.starts_with("msb_krun")
}

/// The workspace's graph for `target`, every workspace feature on: the
/// widest graph any feature combination could reach.
fn resolve(target: &str) -> Metadata {
    Metadata::resolve(target, Features::All)
}

#[test]
fn no_mobile_build_resolves_microsandbox() {
    let mut report = Vec::new();
    for target in MOBILE_TARGETS {
        let meta = resolve(target);
        let graph = meta.graph();
        for id in graph.reachable(&graph.members) {
            let name = graph.nodes[id].name;
            if is_microsandbox(name) {
                report.push(format!("{target}: {name}"));
            }
        }
    }
    report.sort();
    assert!(
        report.is_empty(),
        "microsandbox reaches a mobile build. It may only be a dependency of \
         {SANDBOX_CRATE}, under its desktop target gate:\n  {}",
        report.join("\n  ")
    );
}

#[test]
fn only_the_sandbox_crate_depends_on_microsandbox() {
    for target in DESKTOP_TARGETS {
        let meta = resolve(target);
        let graph = meta.graph();
        let mut direct = Vec::new();
        for member in &graph.members {
            let node = &graph.nodes[member];
            if node
                .deps
                .iter()
                .any(|d| is_microsandbox(graph.nodes[d].name))
            {
                direct.push(node.name);
            }
        }
        assert_eq!(
            direct,
            [SANDBOX_CRATE],
            "{target}: microsandbox must be a dependency of {SANDBOX_CRATE} \
             and no other workspace crate"
        );
    }
}

/// Dave depends on agentium-sandbox in every build, but only its `sandbox`
/// feature turns on the microVM runner, so a default desktop build stays
/// free of microsandbox's graph.
#[test]
fn a_default_desktop_build_compiles_no_microsandbox() {
    for target in DESKTOP_TARGETS {
        let meta = Metadata::resolve(target, Features::Default);
        let graph = meta.graph();
        let reached: Vec<&str> = graph
            .reachable(&graph.members)
            .into_iter()
            .map(|id| graph.nodes[id].name)
            .filter(|name| is_microsandbox(name))
            .collect();
        assert!(
            reached.is_empty(),
            "{target}: a default build resolves {reached:?}; only a `sandbox` \
             (notedeck_chrome, notedeck_dave) or `microvm` (agentium-sandbox) \
             feature may turn microsandbox on"
        );
    }
}

#[test]
fn ios_safe_crates_never_reach_the_sandbox_crate() {
    for target in DESKTOP_TARGETS.iter().chain(MOBILE_TARGETS) {
        let meta = resolve(target);
        let graph = meta.graph();
        for safe in IOS_SAFE {
            let root = graph
                .members
                .iter()
                .copied()
                .find(|m| graph.nodes[m].name == *safe)
                .unwrap_or_else(|| panic!("{safe} is a workspace member"));
            let reaches = graph.reachable(&[root]).into_iter().any(|id| {
                let name = graph.nodes[id].name;
                name == SANDBOX_CRATE || is_microsandbox(name)
            });
            assert!(
                !reaches,
                "{target}: {safe} reaches {SANDBOX_CRATE}; it must stay iOS-safe"
            );
        }
    }
}

#[test]
fn microsandbox_never_downloads_into_home_at_build_time() {
    for target in DESKTOP_TARGETS {
        let meta = resolve(target);
        let graph = meta.graph();
        let top = graph
            .nodes
            .values()
            .find(|n| n.name == "microsandbox")
            .unwrap_or_else(|| panic!("{target}: microsandbox resolves on desktop"));
        assert!(
            !top.features.contains(&"prebuilt"),
            "{target}: microsandbox's `prebuilt` feature is on, so its build \
             script downloads msb + libkrunfw into ~/.microsandbox. Keep its \
             default features off (see crates/agentium-sandbox/Cargo.toml)"
        );
    }
}

/// The shared walk counts a root as reachable from itself, which the
/// iOS-safe check above relies on to see a root that is itself the sandbox
/// crate.
#[test]
fn reachable_includes_its_roots() {
    let meta = resolve(DESKTOP_TARGETS[0]);
    let graph = meta.graph();
    let core = graph.ids_named(IOS_SAFE);
    assert_eq!(core.len(), 1, "agentium-core resolves once");
    assert!(graph.reachable(&core).contains(core[0]));
}
