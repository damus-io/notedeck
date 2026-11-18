//! Boot a real microVM. Needs the microsandbox runtime (see
//! `agentium_sandbox::runtime`), hardware virtualisation (/dev/kvm, or
//! Apple Silicon) and network for the image pull, so it's opt-in:
//!
//! `cargo test -p agentium-sandbox --features microvm --test boot -- --ignored`
#![cfg(microvm)]

use agentium_sandbox::{boot, image, runtime};

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
/// write. The default image has git; git trusts the checkout through the
/// plan's `git_env` although the guest's root doesn't own the files.
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
                .image(image::DEFAULT_IMAGE)
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

/// The whole chain on this checkout: the default image, this build's
/// `notedeck-agent` copied in before boot, the checkout mounted at its host
/// path, a session that passes the build handshake, and one tool call of
/// each kind that matters (pure Rust and shelling out).
///
/// Run `scripts/build-guest-agent` first: the guest is this checkout's dev
/// build.
#[tokio::test]
#[ignore = "boots a microVM: needs the runtime, KVM or Apple Silicon, network, and the guest build"]
async fn the_agent_runs_in_the_default_image_with_no_install_step() {
    use agentium_sandbox::egress::{self, Egress};
    use agentium_sandbox::exec;
    use agentium_sandbox::image::GuestSetup;
    use agentium_sandbox::mounts::{GitAccess, MountPlan};
    use microsandbox::Sandbox;
    use notedeck_agent_proto::call::{BashInput, CodingCall, ReadInput};

    let setup = GuestSetup::for_host(None).expect("scripts/build-guest-agent was run");
    let checkout = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let plan = MountPlan::for_checkout(&checkout, GitAccess::Review).expect("plan");

    /// What the guest answered, kept past the sandbox's cleanup.
    struct Answers {
        read: Result<String, agentium_sandbox::client::CallError>,
        git: Result<String, agentium_sandbox::client::CallError>,
    }

    let name = "agentium-sandbox-agent";
    let started = std::time::Instant::now();
    let egress = Egress::coding();
    let sandbox = egress
        .apply(setup.apply(plan.apply(Sandbox::builder(name).cpus(1).memory(512).replace())))
        .expect("egress policy")
        .create()
        .await
        .expect("guest boots");
    eprintln!("booted with the agent in {:?}", started.elapsed());

    let ran = async {
        let probe = egress.probe_host().expect("coding allowlist has a host");
        let dns = egress::check_dns(&sandbox, probe).await?;
        eprintln!("boot DNS check: {dns:?}");
        let client = exec::start(&sandbox, image::AGENT_PATH, &plan.checkout).await?;
        let read = client
            .call(CodingCall::Read(ReadInput {
                file_path: plan.checkout.join("Cargo.toml").to_string_lossy().into(),
                offset: None,
                limit: Some(3),
            }))
            .result()
            .await;
        let git = client
            .call(CodingCall::Bash(BashInput {
                command: "git rev-parse --is-inside-work-tree".into(),
                description: None,
                timeout: None,
            }))
            .result()
            .await;
        client.shutdown(std::time::Duration::from_secs(2)).await;
        Ok::<_, agentium_sandbox::SandboxError>(Answers { read, git })
    }
    .await;
    let _ = sandbox.stop().await;
    let _ = Sandbox::remove(name).await;

    let Answers { read, git } = ran.expect("session with this build's agent");
    let read = read.expect("Read in the guest");
    assert!(read.contains("[workspace]"), "{read}");
    let git = git.expect("Bash in the guest");
    assert!(git.contains("true"), "{git}");
}

/// The default allowlist under a real guest: its probe host resolves and
/// is reachable on 443, a name off the list is NXDOMAIN at once (not the
/// spike's 70-second hang), and an allowlisted host on another port is
/// refused. The one-shot form of what `egress`'s unit tests ask the
/// policy.
#[tokio::test]
#[ignore = "boots a microVM: needs the runtime, KVM or Apple Silicon, and network"]
async fn the_allowlist_resolves_its_hosts_and_refuses_the_rest_fast() {
    use agentium_sandbox::egress::{self, DnsCheck, Egress};
    use microsandbox::Sandbox;
    use std::time::{Duration, Instant};

    let egress = Egress::coding();
    let name = "agentium-sandbox-egress";
    let sandbox = egress
        .apply(
            Sandbox::builder(name)
                .image(image::DEFAULT_IMAGE)
                .cpus(1)
                .memory(256)
                .replace(),
        )
        .expect("egress policy")
        .create()
        .await
        .expect("guest boots");

    let allowed = egress::check_dns(&sandbox, "github.com").await;
    let started = Instant::now();
    let denied = egress::check_dns(&sandbox, "example.com").await;
    let denied_after = started.elapsed();
    // git is the one HTTPS client alpine/git is sure to have.
    let https = sandbox
        .shell("git ls-remote https://github.com/git/git HEAD")
        .await;
    let off_port = sandbox
        .shell("nc -w 5 github.com 80 </dev/null && echo open || echo refused")
        .await;
    let _ = sandbox.stop().await;
    let _ = Sandbox::remove(name).await;

    assert!(
        matches!(allowed, Ok(DnsCheck::Resolved { .. })),
        "{allowed:?}"
    );
    assert!(denied.is_err(), "example.com resolved: {denied:?}");
    assert!(
        denied_after < Duration::from_secs(5),
        "a denied lookup took {denied_after:?}"
    );
    let https = https.expect("exec");
    assert!(
        https.status().success,
        "git over https: {}",
        String::from_utf8_lossy(https.stderr_bytes())
    );
    let off_port = off_port.expect("exec");
    assert_eq!(
        String::from_utf8_lossy(off_port.stdout_bytes()).trim(),
        "refused"
    );
}
