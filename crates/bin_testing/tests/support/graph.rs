//! `cargo metadata` and the resolved dependency graph, for the tests that
//! check what a build pulls in (`guest_no_c`, `sandbox_desktop_only`,
//! `cdylibs`).
//!
//! It lives here rather than in bin_testing's `src/`: the library is std-only
//! by design, and serde_json is only a dev-dependency.

// Each test target uses a subset of this module, so the rest is dead code
// in that target.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::process::Command;

use serde_json::Value;

/// `cargo metadata --format-version 1 <args>`, run from this crate, parsed.
/// Panics with cargo's stderr if it fails.
pub fn metadata(args: &[&str]) -> Value {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["metadata", "--format-version", "1"])
        .args(args)
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

/// Which features the graph is resolved with. The choice changes what a
/// test checks: `Default` is what a plain build of the roots resolves,
/// `All` is the widest graph any feature combination could reach.
#[derive(Clone, Copy, Debug)]
pub enum Features {
    /// Each crate's default features, as `cargo build` resolves them.
    Default,
    /// `--all-features` across the workspace.
    All,
}

/// `cargo metadata` resolved for one target platform. Owns the json the
/// [`Graph`] borrows from.
pub struct Metadata(Value);

impl Metadata {
    /// The workspace's dependency graph resolved for `target` (a rustc
    /// triple) with `features`.
    pub fn resolve(target: &str, features: Features) -> Self {
        let mut args = vec!["--filter-platform", target];
        if let Features::All = features {
            args.push("--all-features");
        }
        Self(metadata(&args))
    }

    /// The resolved packages, keyed by package id.
    pub fn graph(&self) -> Graph<'_> {
        let packages: HashMap<&str, &Value> = self.0["packages"]
            .as_array()
            .expect("packages")
            .iter()
            .map(|p| (p["id"].as_str().expect("package id"), p))
            .collect();
        let nodes = self.0["resolve"]["nodes"]
            .as_array()
            .expect("resolve nodes")
            .iter()
            .map(|n| {
                let id = n["id"].as_str().expect("node id");
                let package = packages[id];
                let node = Node {
                    name: package["name"].as_str().expect("name"),
                    links: package["links"].as_str(),
                    features: strs(&n["features"]).collect(),
                    deps: built_deps(n).collect(),
                };
                (id, node)
            })
            .collect();
        let members = strs(&self.0["workspace_members"]).collect();
        Graph { nodes, members }
    }
}

/// A resolved dependency graph: every package one target builds, by id.
pub struct Graph<'a> {
    /// Every resolved package, keyed by package id.
    pub nodes: HashMap<&'a str, Node<'a>>,
    /// The ids of the workspace's own crates.
    pub members: Vec<&'a str>,
}

/// One resolved package.
pub struct Node<'a> {
    /// The crate's package name.
    pub name: &'a str,
    /// The native library it declares with `links`, if any.
    pub links: Option<&'a str>,
    /// The features it is resolved with.
    pub features: Vec<&'a str>,
    /// The ids it builds against: normal and build dependencies. Dev ones
    /// never reach a shipped build, so they are left out.
    pub deps: Vec<&'a str>,
}

impl<'a> Graph<'a> {
    /// Every id reachable from `roots` (the roots included) through normal
    /// and build dependencies.
    pub fn reachable(&self, roots: &[&'a str]) -> HashSet<&'a str> {
        let mut seen = HashSet::new();
        let mut queue = roots.to_vec();
        while let Some(id) = queue.pop() {
            if seen.insert(id) {
                queue.extend(self.nodes[id].deps.iter().copied());
            }
        }
        seen
    }

    /// The ids of the resolved packages named one of `names`, sorted. A name
    /// that resolves twice (two versions) gives two ids.
    pub fn ids_named(&self, names: &[&str]) -> Vec<&'a str> {
        let mut ids: Vec<&'a str> = self
            .nodes
            .iter()
            .filter(|(_, node)| names.contains(&node.name))
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        ids
    }
}

/// The strings of a json array.
fn strs(array: &Value) -> impl Iterator<Item = &str> {
    array
        .as_array()
        .expect("array")
        .iter()
        .map(|s| s.as_str().expect("string"))
}

/// A resolve node's dependency ids, minus those it only dev-depends on.
fn built_deps(node: &Value) -> impl Iterator<Item = &str> {
    node["deps"]
        .as_array()
        .expect("deps")
        .iter()
        .filter(|d| {
            d["dep_kinds"]
                .as_array()
                .expect("dep_kinds")
                .iter()
                .any(|k| k["kind"].as_str() != Some("dev"))
        })
        .map(|d| d["pkg"].as_str().expect("pkg"))
}
