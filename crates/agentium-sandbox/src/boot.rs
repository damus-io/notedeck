//! Boot a guest and run one command in it: the end-to-end check that this
//! machine can run sandboxes at all (runtime present, hardware
//! virtualisation on, image pullable, guest arch right).

use std::time::{Duration, Instant};

use microsandbox::Sandbox;

use crate::SandboxError;

/// What a smoke boot saw.
#[derive(Debug, Clone)]
pub struct BootReport {
    /// `uname -m` inside the guest. libkrun doesn't emulate, so this is the
    /// host's architecture.
    pub guest_arch: String,
    /// From asking for the sandbox to it answering the command, including
    /// an image pull on first use.
    pub elapsed: Duration,
}

/// Boot `image` as a sandbox called `name`, run `uname -m`, then stop and
/// remove it. A sandbox of the same name is replaced; this one is removed
/// even when the command fails, because a stopped sandbox's record makes
/// the next create of that name fail.
pub async fn smoke_boot(name: &str, image: &str) -> Result<BootReport, SandboxError> {
    let started = Instant::now();
    let sandbox = Sandbox::builder(name)
        .image(image)
        .cpus(1)
        .memory(256)
        .replace()
        .create()
        .await?;

    let ran = sandbox.shell("uname -m").await;
    let stopped = sandbox.stop().await;
    let removed = Sandbox::remove(name).await;

    let output = ran?;
    stopped?;
    removed?;
    let status = output.status();
    if !status.success {
        return Err(SandboxError::GuestCommand {
            code: status.code,
            stderr: String::from_utf8_lossy(output.stderr_bytes()).into_owned(),
        });
    }
    Ok(BootReport {
        guest_arch: String::from_utf8_lossy(output.stdout_bytes())
            .trim()
            .to_owned(),
        elapsed: started.elapsed(),
    })
}
