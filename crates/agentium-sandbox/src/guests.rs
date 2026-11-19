//! The microsandbox [`VmHost`]: guest lifecycles ([`crate::fleet`]) on
//! real microVMs, and the `Guests` and `Guest` notedeck uses.
//!
//! The order a guest is booted and torn down in, the sweep, and the reap
//! of a dropped guest are [`crate::fleet`]'s, and tested there without
//! KVM. What is left here is each [`VmHost`] call as microsandbox spells
//! it; `tests/boot.rs` runs them on real hardware.

use std::time::Duration;

use microsandbox::sandbox::{SandboxHandle, SandboxStatus};
use microsandbox::{MicrosandboxError, Sandbox};

use crate::client::GuestClient;
use crate::egress;
use crate::fleet::{self, VmHost, VmPage, VmRecord};
use crate::image::AGENT_PATH;
use crate::lifecycle::{GuestLabel, APP, LABEL_APP};
use crate::SandboxError;

pub use crate::fleet::{GuestSpec, AGENT_GRACE, STOP_TIMEOUT};

/// How many sandboxes a sweep asks microsandbox for per page (its max).
const SWEEP_PAGE: u32 = 100;

/// The guests one notedeck runs, on microsandbox.
pub type Guests = fleet::Guests<Microsandbox>;

/// A booted microsandbox guest; see [`fleet::Guest`].
pub type Guest = fleet::Guest<Microsandbox>;

/// microsandbox, as the [`VmHost`] guests run on. Stateless: every call
/// goes to the runtime under `~/.microsandbox`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Microsandbox;

impl fleet::Guests<Microsandbox> {
    /// At most `max` guests at once ([`crate::lifecycle::DEFAULT_MAX_GUESTS`]
    /// is the usual), owned by this process.
    pub fn new(max: usize) -> Self {
        Self::with_host(Microsandbox, max)
    }
}

impl VmHost for Microsandbox {
    type Vm = Sandbox;

    async fn get(&self, name: &str) -> Result<Option<VmRecord>, SandboxError> {
        match Sandbox::get(name).await {
            Ok(handle) => Ok(Some(record(&handle))),
            Err(MicrosandboxError::SandboxNotFound(_)) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn list_ours(&self, cursor: Option<String>) -> Result<VmPage, SandboxError> {
        let page = Sandbox::list_with(|list| {
            let list = list.limit(SWEEP_PAGE).label(LABEL_APP, APP);
            match cursor {
                Some(c) => list.cursor(c),
                None => list,
            }
        })
        .await?;
        Ok(VmPage {
            records: page.sandboxes.iter().map(record).collect(),
            next: page.next_cursor,
        })
    }

    async fn create(
        &self,
        name: &str,
        labels: &[GuestLabel],
        spec: &GuestSpec<'_>,
    ) -> Result<Sandbox, SandboxError> {
        let builder = Sandbox::builder(name)
            .cpus(spec.cpus)
            .memory(spec.memory_mib)
            .labels(labels.iter().map(|l| (l.key, l.value.as_str())));
        let builder = spec.setup.apply(spec.plan.apply(builder));
        let builder = match spec.ipc {
            Some(ipc) => ipc.apply(builder),
            None => builder,
        };
        // Neither `.replace()` nor `.detached(true)`: see Guests::start.
        Ok(spec.egress.apply(builder)?.create().await?)
    }

    async fn connect(
        &self,
        vm: &Sandbox,
        spec: &GuestSpec<'_>,
    ) -> Result<GuestClient, SandboxError> {
        if let Some(probe) = spec.egress.probe_host() {
            egress::check_dns(vm, probe).await?;
        }
        crate::exec::start(vm, AGENT_PATH, &spec.plan.checkout, spec.ipc).await
    }

    async fn stop(&self, vm: &Sandbox, timeout: Duration) -> Result<(), SandboxError> {
        Ok(vm.stop_with_timeout(timeout).await?)
    }

    async fn stop_named(&self, name: &str, timeout: Duration) -> Result<(), SandboxError> {
        match Sandbox::get(name).await {
            Ok(handle) => Ok(handle.stop_with_timeout(timeout).await?),
            Err(MicrosandboxError::SandboxNotFound(_)) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn remove(&self, name: &str) -> Result<(), SandboxError> {
        match Sandbox::remove(name).await {
            Ok(()) | Err(MicrosandboxError::SandboxNotFound(_)) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn owner_alive(&self, pid: u32) -> bool {
        process_alive(pid)
    }
}

/// Stop the sandbox `name` if it's up (killing it if a clean stop takes
/// longer than [`STOP_TIMEOUT`]), then remove its record. Already gone is
/// success: another notedeck's sweep may have got there first.
pub async fn reap(name: &str) -> Result<(), SandboxError> {
    fleet::reap(&Microsandbox, name).await
}

/// What the sweep and the stale check read off a sandbox.
fn record(handle: &SandboxHandle) -> VmRecord {
    VmRecord {
        name: handle.name().to_owned(),
        labels: handle.config().map(|c| c.spec.labels).unwrap_or_default(),
        running: is_up(handle.status_snapshot()),
    }
}

/// Anything but stopped or crashed: microsandbox's own idea of terminal.
fn is_up(status: SandboxStatus) -> bool {
    !matches!(status, SandboxStatus::Stopped | SandboxStatus::Crashed)
}

/// Whether a process with this pid exists. `EPERM` means it exists and
/// isn't ours to signal: alive.
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 sends nothing; it only checks the pid exists.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_alive_and_a_reaped_child_is_not() {
        assert!(process_alive(std::process::id()));
        let mut child = std::process::Command::new("true").spawn().expect("spawn");
        let pid = child.id();
        child.wait().expect("wait");
        assert!(!process_alive(pid));
        assert!(!process_alive(0), "0 is the process group, not a pid");
        assert!(!process_alive(u32::MAX));
    }

    #[test]
    fn stopped_and_crashed_are_down() {
        assert!(!is_up(SandboxStatus::Stopped));
        assert!(!is_up(SandboxStatus::Crashed));
        for up in [
            SandboxStatus::Created,
            SandboxStatus::Starting,
            SandboxStatus::Running,
            SandboxStatus::Draining,
            SandboxStatus::Paused,
        ] {
            assert!(is_up(up), "{up:?}");
        }
    }
}
