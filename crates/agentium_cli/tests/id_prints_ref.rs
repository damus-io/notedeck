//! End-to-end check for `agentium id` over the real binary: it turns a
//! session's d-tag into its `agentium:` ref with no key, cache or relay, so a
//! script (e.g. `scripts/headway-live-rig`) can name a session it only knows by
//! d-tag.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// *This* worktree's freshly-built `agentium`, not a sibling's leftover on the
/// shared `target/debug/agentium`. See [`bin_testing::worktree_bin`].
fn agentium_bin() -> PathBuf {
    bin_testing::worktree_bin(
        Path::new(env!("CARGO_BIN_EXE_agentium")),
        "agentium_cli",
        is_current_build,
    )
}

/// A current build lists the `id` command in its no-arg usage (on stderr).
fn is_current_build(bin: &Path) -> bool {
    let Ok(out) = Command::new(bin).output() else {
        return false;
    };
    String::from_utf8_lossy(&out.stderr).contains("id <d-tag")
}

#[test]
fn id_prints_each_d_tags_ref_without_a_key_or_relay() {
    // An empty data dir: no stored key, no stored relay, no cache. The relay
    // is a closed port, which `id` must never dial.
    let dir = TempDir::new().expect("tmp dir");
    let out = Command::new(agentium_bin())
        .args(["--relay", "ws://127.0.0.1:1", "id", "d-one", "d-two"])
        .env("XDG_DATA_HOME", dir.path())
        .env("HOME", dir.path())
        .env_remove("AGENTIUM_NSEC")
        .output()
        .expect("run agentium");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let expected = format!(
        "{}\n{}\n",
        agentium_core::wordid::session_ref("d-one"),
        agentium_core::wordid::session_ref("d-two"),
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
}
