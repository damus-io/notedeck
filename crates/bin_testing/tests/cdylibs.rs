//! No workspace crate that a test links may be a `cdylib` (or `dylib`); the
//! Android library is `crates/notedeck_android`, which nothing depends on.
//!
//! Cargo names a path crate's cdylib without the hash it gives every other
//! build, so it keeps one build of that crate per profile. Every
//! `cargo test -p <crate>` that resolves the crate with other features, or
//! links it against a dependency built with other features, rebuilds it over
//! the last one. notedeck_chrome and notedeck_columns were
//! `["lib", "cdylib"]`, and each snapshot suite's dev-dependencies resolve
//! them differently, so back-to-back suites rebuilt both every time
//! (headway:notedeck/brass-suggest-satisfy).
//!
//! `cargo test -p bin_testing --test cdylibs`

use std::process::Command;

/// The one crate allowed a shared-library target: the leaf Android loads.
const ANDROID_LIB: &str = "notedeck_android";

/// Crate types whose output cargo names without a hash.
const UNHASHED: &[&str] = &["cdylib", "dylib"];

/// One workspace target that builds a shared library.
struct SharedLib {
    package: String,
    target: String,
}

/// Every workspace target with a `cdylib` or `dylib` crate type.
fn shared_libs() -> Vec<SharedLib> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args([
            "metadata",
            "--no-deps",
            "--offline",
            "--format-version",
            "1",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo metadata runs");
    assert!(
        out.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).expect("metadata json");

    let mut libs = Vec::new();
    for pkg in meta["packages"].as_array().expect("packages") {
        for target in pkg["targets"].as_array().expect("targets") {
            let shared = target["crate_types"]
                .as_array()
                .expect("crate_types")
                .iter()
                .any(|t| UNHASHED.contains(&t.as_str().unwrap_or_default()));
            if !shared {
                continue;
            }
            libs.push(SharedLib {
                package: pkg["name"].as_str().expect("name").to_string(),
                target: target["name"].as_str().expect("target name").to_string(),
            });
        }
    }
    libs
}

#[test]
fn only_the_android_leaf_is_a_shared_library() {
    let libs = shared_libs();
    let offenders: Vec<String> = libs
        .iter()
        .filter(|lib| lib.package != ANDROID_LIB)
        .map(|lib| format!("{} (target {})", lib.package, lib.target))
        .collect();
    assert!(
        offenders.is_empty(),
        "these crates build a cdylib/dylib, so every test build that resolves \
         them with other features rebuilds them over the last one; give the \
         shared library its own leaf crate, as {ANDROID_LIB} is:\n  {}",
        offenders.join("\n  ")
    );
    assert!(
        libs.iter().any(|lib| lib.package == ANDROID_LIB),
        "{ANDROID_LIB} must stay a cdylib: it is the .so the Android app loads"
    );
}
