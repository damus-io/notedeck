//! Boot a real microVM. Needs the microsandbox runtime (see
//! `agentium_sandbox::runtime`), hardware virtualisation (/dev/kvm, or
//! Apple Silicon) and network for the image pull, so it's opt-in:
//!
//! `cargo test -p agentium-sandbox --features microvm --test boot -- --ignored`
#![cfg(microvm)]

use agentium_sandbox::{boot, runtime};

#[tokio::test]
#[ignore = "boots a microVM: needs the runtime, KVM or Apple Silicon, and network"]
async fn a_guest_boots_and_runs_a_command() {
    let files = runtime::resolve().expect("runtime installed (runtime::install)");
    eprintln!(
        "msb {} / {}",
        files.msb.display(),
        files.libkrunfw.display()
    );

    let report = boot::smoke_boot("agentium-sandbox-smoke", "alpine")
        .await
        .expect("guest boots");
    eprintln!(
        "booted in {:?}, guest arch {}",
        report.elapsed, report.guest_arch
    );
    assert_eq!(report.guest_arch, std::env::consts::ARCH);
}
