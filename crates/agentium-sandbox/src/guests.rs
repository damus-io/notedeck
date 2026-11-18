//! The one owner of guest lifecycles: boot a guest for a session in the
//! only order that works, tear it down so nothing is left, and sweep up
//! what a crash left behind. The rules (naming, labels, when a guest may
//! be reaped, the bound) are in [`crate::lifecycle`]; this is them applied
//! to microsandbox.
//!
//! ```text
//!   Guests::sweep()          once at startup, before any start
//!   Guests::start(spec) ─▶ Guest ─┬─ shutdown().await   session ended or deleted
//!                                 └─ drop               task aborted: reaped in the background
//! ```
//!
//! A [`Guest`] that is dropped without [`Guest::shutdown`] (its session's
//! task was aborted, a `?` returned early) spawns its own stop-and-remove
//! on the current tokio runtime, keeping its [`Slot`] until that finishes.
//! With no runtime to spawn on, the VM still shuts down when notedeck
//! exits (attached mode) and the next [`Guests::sweep`] removes its
//! record.

use std::path::Path;
use std::time::Duration;

use microsandbox::sandbox::SandboxStatus;
use microsandbox::{MicrosandboxError, Sandbox};

use crate::client::GuestClient;
use crate::egress::{self, Egress};
use crate::image::{GuestSetup, AGENT_PATH};
use crate::ipc::NotedeckIpc;
use crate::lifecycle::{
    guest_name, labels, verdict, Capacity, GuestRecord, Slot, SweepFailure, SweepReport, Verdict,
    APP, LABEL_APP,
};
use crate::mounts::MountPlan;
use crate::SandboxError;

/// How long a guest gets to power off cleanly before it is killed.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the executor gets to finish its calls at shutdown.
pub const AGENT_GRACE: Duration = Duration::from_secs(2);

/// How many sandboxes a sweep asks microsandbox for per page (its max).
const SWEEP_PAGE: u32 = 100;

/// Everything a session's guest is booted from.
#[derive(Debug, Clone, Copy)]
pub struct GuestSpec<'a> {
    /// The dave session the guest serves; it names the guest
    /// ([`guest_name`]).
    pub session_id: &'a str,
    /// The checkout and git dir to mount.
    pub plan: &'a MountPlan,
    /// The image and the agent binary to copy in.
    pub setup: &'a GuestSetup,
    /// Where the guest may connect.
    pub egress: &'a Egress,
    /// The opt-in route to notedeck's socket ([`crate::ipc`]); `None`, the
    /// default, leaves the guest with no way to act as the user.
    pub ipc: Option<&'a NotedeckIpc>,
    /// Virtual CPUs.
    pub cpus: u8,
    /// Memory, in MiB.
    pub memory_mib: u32,
}

/// The guests one notedeck runs: the bound on how many, and who owns them.
#[derive(Debug, Clone)]
pub struct Guests {
    capacity: Capacity,
    owner_pid: u32,
}

/// A booted guest with a session open to its executor. Shut it down with
/// [`Guest::shutdown`]; dropping it reaps it in the background instead.
pub struct Guest {
    name: String,
    session_id: String,
    /// `None` only during teardown.
    sandbox: Option<Sandbox>,
    /// `None` until the executor is up, and during teardown.
    client: Option<GuestClient>,
    /// Given back when the guest is gone, not when this is dropped.
    slot: Option<Slot>,
}

impl Guests {
    /// At most `max` guests at once ([`crate::lifecycle::DEFAULT_MAX_GUESTS`]
    /// is the usual), owned by this process.
    pub fn new(max: usize) -> Self {
        Self {
            capacity: Capacity::new(max),
            owner_pid: std::process::id(),
        }
    }

    /// The bound, and how much of it is in use.
    pub fn capacity(&self) -> &Capacity {
        &self.capacity
    }

    /// Boot a guest for `spec.session_id` and open a session with its
    /// executor, in the one order that works: mounts, image and agent,
    /// the route to notedeck if any, egress; create; check DNS under the
    /// egress policy; start the executor. A failure after the create tears
    /// the guest down before returning.
    ///
    /// Refused with [`SandboxError::AtCapacity`] when the bound is reached,
    /// [`SandboxError::GuestRunning`] when the session already has a live
    /// guest (here or in another notedeck), and
    /// [`SandboxError::GuestNameTaken`] when a sandbox that isn't ours has
    /// the name. A stale record of the session's guest (a crash, a stop
    /// without a remove) is reaped first.
    pub async fn start(&self, spec: GuestSpec<'_>) -> Result<Guest, SandboxError> {
        let name = guest_name(spec.session_id)?;
        let slot = self.capacity.try_take()?;
        self.clear_stale(&name, spec.session_id).await?;

        let builder = Sandbox::builder(&name)
            .cpus(spec.cpus)
            .memory(spec.memory_mib)
            .labels(
                labels(spec.session_id, self.owner_pid)
                    .into_iter()
                    .map(|l| (l.key, l.value)),
            );
        let builder = spec.setup.apply(spec.plan.apply(builder));
        let builder = match spec.ipc {
            Some(ipc) => ipc.apply(builder),
            None => builder,
        };
        // Neither `.replace()` (it would tear down another notedeck's live
        // guest of the same session) nor `.detached(true)` (see
        // crate::lifecycle).
        let sandbox = spec.egress.apply(builder)?.create().await?;

        let mut guest = Guest {
            name,
            session_id: spec.session_id.to_owned(),
            sandbox: Some(sandbox),
            client: None,
            slot: Some(slot),
        };
        match connect(&guest, spec.egress, &spec.plan.checkout, spec.ipc).await {
            Ok(client) => {
                guest.client = Some(client);
                Ok(guest)
            }
            Err(e) => {
                // The boot error is the one worth reporting.
                let _ = guest.shutdown().await;
                Err(e)
            }
        }
    }

    /// Reap every guest of ours whose owner is gone, and every stopped one.
    /// Run it once at startup, before the first [`Guests::start`]; it is
    /// safe to run again any time, since this process's own running guests
    /// and other live notedecks' are kept ([`verdict`]).
    ///
    /// Errs only if microsandbox can't list sandboxes at all (no runtime);
    /// a guest that can't be reaped is in [`SweepReport::failed`].
    pub async fn sweep(&self) -> Result<SweepReport, SandboxError> {
        let mut report = SweepReport::default();
        let mut cursor = None;
        loop {
            let page = Sandbox::list_with(|list| {
                let list = list.limit(SWEEP_PAGE).label(LABEL_APP, APP);
                match cursor.take() {
                    Some(c) => list.cursor(c),
                    None => list,
                }
            })
            .await?;
            for handle in &page.sandboxes {
                let labels = handle.config().map(|c| c.spec.labels).unwrap_or_default();
                let record = GuestRecord {
                    name: handle.name(),
                    labels: &labels,
                    running: is_up(handle.status_snapshot()),
                };
                match verdict(&record, self.owner_pid, process_alive) {
                    Verdict::NotOurs => {}
                    Verdict::Keep(_) => report.kept.push(handle.name().to_owned()),
                    Verdict::Reap => match reap(handle.name()).await {
                        Ok(()) => report.reaped.push(handle.name().to_owned()),
                        Err(e) => report.failed.push(SweepFailure {
                            name: handle.name().to_owned(),
                            error: e.to_string(),
                        }),
                    },
                }
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return Ok(report),
            }
        }
    }

    /// Make room for `name`: nothing there, or a record of ours to reap.
    async fn clear_stale(&self, name: &str, session_id: &str) -> Result<(), SandboxError> {
        let handle = match Sandbox::get(name).await {
            Ok(handle) => handle,
            Err(MicrosandboxError::SandboxNotFound(_)) => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let labels = handle.config().map(|c| c.spec.labels).unwrap_or_default();
        let record = GuestRecord {
            name,
            labels: &labels,
            running: is_up(handle.status_snapshot()),
        };
        match verdict(&record, self.owner_pid, process_alive) {
            Verdict::NotOurs => Err(SandboxError::GuestNameTaken(name.to_owned())),
            Verdict::Keep(_) => Err(SandboxError::GuestRunning(session_id.to_owned())),
            Verdict::Reap => reap(name).await.map_err(SandboxError::from),
        }
    }
}

impl Default for Guests {
    fn default() -> Self {
        Self::new(crate::lifecycle::DEFAULT_MAX_GUESTS)
    }
}

/// Check DNS, then start the executor: the part of a boot that runs in a
/// created guest, so its failure needs a teardown.
async fn connect(
    guest: &Guest,
    egress: &Egress,
    checkout: &Path,
    ipc: Option<&NotedeckIpc>,
) -> Result<GuestClient, SandboxError> {
    let sandbox = guest.sandbox();
    if let Some(probe) = egress.probe_host() {
        egress::check_dns(sandbox, probe).await?;
    }
    crate::exec::start(sandbox, AGENT_PATH, checkout, ipc).await
}

impl Guest {
    /// The sandbox's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The dave session this guest serves.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The live sandbox, for anything beyond tool calls.
    pub fn sandbox(&self) -> &Sandbox {
        self.sandbox
            .as_ref()
            .expect("a guest has its sandbox until teardown")
    }

    /// The session with the guest's executor.
    pub fn client(&self) -> &GuestClient {
        self.client
            .as_ref()
            .expect("a started guest has its executor session")
    }

    /// End the executor's session, stop the guest and remove its record.
    /// The guest's slot is free once this returns, whatever it returns.
    pub async fn shutdown(mut self) -> Result<(), SandboxError> {
        if let Some(client) = self.client.take() {
            client.shutdown(AGENT_GRACE).await;
        }
        let sandbox = self.sandbox.take();
        let slot = self.slot.take();
        let result = teardown(&self.name, sandbox).await;
        drop(slot);
        result.map_err(SandboxError::from)
    }
}

impl Drop for Guest {
    /// Reap a guest nobody shut down, in the background. Its slot goes with
    /// the task, so it still counts until it's gone.
    fn drop(&mut self) {
        let Some(sandbox) = self.sandbox.take() else {
            return;
        };
        let client = self.client.take();
        let slot = self.slot.take();
        let name = std::mem::take(&mut self.name);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            // Attached mode shuts the VM down when notedeck exits; the next
            // sweep removes the record.
            return;
        };
        runtime.spawn(async move {
            drop(client);
            if let Err(e) = teardown(&name, Some(sandbox)).await {
                eprintln!("agentium-sandbox: reaping dropped guest {name}: {e}");
            }
            drop(slot);
        });
    }
}

/// Stop the live `sandbox` if we have it, then reap `name` by name, so a
/// stop that failed still gets a kill and the record still goes.
async fn teardown(name: &str, sandbox: Option<Sandbox>) -> Result<(), MicrosandboxError> {
    if let Some(sandbox) = sandbox {
        let _ = sandbox.stop_with_timeout(STOP_TIMEOUT).await;
    }
    reap(name).await
}

/// Stop the sandbox `name` if it's up (killing it if a clean stop takes
/// longer than [`STOP_TIMEOUT`]), then remove its record. Already gone is
/// success: another notedeck's sweep may have got there first.
pub async fn reap(name: &str) -> Result<(), MicrosandboxError> {
    let handle = match Sandbox::get(name).await {
        Ok(handle) => handle,
        Err(MicrosandboxError::SandboxNotFound(_)) => return Ok(()),
        Err(e) => return Err(e),
    };
    if is_up(handle.status_snapshot()) {
        handle.stop_with_timeout(STOP_TIMEOUT).await?;
    }
    match Sandbox::remove(name).await {
        Err(MicrosandboxError::SandboxNotFound(_)) => Ok(()),
        other => other,
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
