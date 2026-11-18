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

/// This checkout, mounted through a Review [`MountPlan`] at its host path:
/// git in the guest sees the host's HEAD, and the shared git dir refuses a
/// write. Needs an image with git in it.
///
/// [`MountPlan`]: agentium_sandbox::mounts::MountPlan
#[tokio::test]
#[ignore = "boots a microVM: needs the runtime, KVM or Apple Silicon, and network"]
async fn this_checkout_mounts_at_its_host_path() {
    use agentium_sandbox::mounts::{GitAccess, MountPlan};
    use microsandbox::Sandbox;

    let checkout = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let plan = MountPlan::for_checkout(&checkout, GitAccess::Review).expect("plan");
    eprintln!("{plan:#?}");
    let host_head = std::process::Command::new("git")
        .arg("-C")
        .arg(&plan.checkout)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("host git");
    let host_head = String::from_utf8_lossy(&host_head.stdout).trim().to_owned();

    let name = "agentium-sandbox-mounts";
    let sandbox = plan
        .apply(
            Sandbox::builder(name)
                .image("alpine/git")
                .cpus(1)
                .memory(512)
                .replace(),
        )
        .create()
        .await
        .expect("guest boots");
    let head = sandbox
        .shell("git rev-parse HEAD && git status --porcelain >/dev/null")
        .await;
    let refused = sandbox
        .shell("git update-ref refs/heads/agentium-probe HEAD")
        .await;
    let _ = sandbox.stop().await;
    let _ = Sandbox::remove(name).await;

    let head = head.expect("exec");
    assert!(
        head.status().success,
        "git in the guest: {}",
        String::from_utf8_lossy(head.stderr_bytes())
    );
    assert_eq!(
        String::from_utf8_lossy(head.stdout_bytes()).trim(),
        host_head
    );
    let refused = refused.expect("exec");
    assert!(!refused.status().success, "Review let a ref write through");
}
