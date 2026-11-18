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
//! - microsandbox's own `prebuilt` feature stays off: it makes microsandbox's
//!   build script download ~54 MB into `~/.microsandbox` on every machine
//!   that compiles it.
//!
//! `cargo test -p bin_testing --test sandbox_desktop_only`

use std::collections::{HashMap, HashSet};
use std::process::Command;

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

/// `cargo metadata` for `target`, every workspace feature on.
struct Graph {
    meta: serde_json::Value,
}

/// One resolved package: its name and the ids it builds against.
struct Node<'a> {
    name: &'a str,
    features: Vec<&'a str>,
    /// Normal and build dependencies; dev ones never reach a shipped build.
    deps: Vec<&'a str>,
}

impl Graph {
    fn new(target: &str) -> Self {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let out = Command::new(cargo)
            .args([
                "metadata",
                "--format-version",
                "1",
                "--all-features",
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
        Self {
            meta: serde_json::from_slice(&out.stdout).expect("metadata json"),
        }
    }

    fn nodes(&self) -> HashMap<&str, Node<'_>> {
        let names: HashMap<&str, &str> = self.meta["packages"]
            .as_array()
            .expect("packages")
            .iter()
            .map(|p| (p["id"].as_str().unwrap(), p["name"].as_str().unwrap()))
            .collect();
        self.meta["resolve"]["nodes"]
            .as_array()
            .expect("resolve nodes")
            .iter()
            .map(|n| {
                let id = n["id"].as_str().unwrap();
                let deps = n["deps"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|d| {
                        d["dep_kinds"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|k| k["kind"].as_str() != Some("dev"))
                    })
                    .map(|d| d["pkg"].as_str().unwrap())
                    .collect();
                let features = n["features"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| f.as_str().unwrap())
                    .collect();
                let node = Node {
                    name: names[id],
                    features,
                    deps,
                };
                (id, node)
            })
            .collect()
    }

    fn members(&self) -> Vec<&str> {
        self.meta["workspace_members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m.as_str().unwrap())
            .collect()
    }
}

/// Every id reachable from `roots` through normal and build dependencies.
fn reachable<'a>(nodes: &HashMap<&'a str, Node<'a>>, roots: &[&'a str]) -> HashSet<&'a str> {
    let mut seen = HashSet::new();
    let mut queue = roots.to_vec();
    while let Some(id) = queue.pop() {
        if seen.insert(id) {
            queue.extend(nodes[id].deps.iter().copied());
        }
    }
    seen
}

#[test]
fn no_mobile_build_resolves_microsandbox() {
    let mut report = Vec::new();
    for target in MOBILE_TARGETS {
        let graph = Graph::new(target);
        let nodes = graph.nodes();
        for id in reachable(&nodes, &graph.members()) {
            if is_microsandbox(nodes[id].name) {
                report.push(format!("{target}: {}", nodes[id].name));
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
        let graph = Graph::new(target);
        let nodes = graph.nodes();
        let mut direct = Vec::new();
        for member in graph.members() {
            let node = &nodes[member];
            if node.deps.iter().any(|d| is_microsandbox(nodes[d].name)) {
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

#[test]
fn ios_safe_crates_never_reach_the_sandbox_crate() {
    for target in DESKTOP_TARGETS.iter().chain(MOBILE_TARGETS) {
        let graph = Graph::new(target);
        let nodes = graph.nodes();
        for safe in IOS_SAFE {
            let root = graph
                .members()
                .into_iter()
                .find(|m| nodes[m].name == *safe)
                .unwrap_or_else(|| panic!("{safe} is a workspace member"));
            let reaches = reachable(&nodes, &[root])
                .into_iter()
                .any(|id| nodes[id].name == SANDBOX_CRATE || is_microsandbox(nodes[id].name));
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
        let graph = Graph::new(target);
        let nodes = graph.nodes();
        let top = nodes
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
