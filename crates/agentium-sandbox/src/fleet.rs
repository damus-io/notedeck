//! The one owner of guest lifecycles: boot a guest for a session in the
//! only order that works, tear it down so nothing is left, and sweep up
//! what a crash left behind. The rules (naming, labels, when a guest may
//! be reaped, the bound) are in [`crate::lifecycle`]; this is them applied
//! to whatever runs the VMs, a [`VmHost`].
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
//!
//! This module is built in every configuration, so the orchestration is
//! tested without KVM against an in-memory host (`FakeVms`, test-only).
//! The real host is microsandbox, in [`crate::guests`] (`microvm` only),
//! which also names the concrete `Guests` and `Guest` notedeck uses.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use crate::client::GuestClient;
use crate::egress::Egress;
use crate::image::GuestSetup;
use crate::ipc::NotedeckIpc;
use crate::lifecycle::{
    guest_name, labels, verdict, Capacity, GuestLabel, GuestRecord, Slot, SweepFailure,
    SweepReport, Verdict,
};
use crate::mounts::MountPlan;
use crate::SandboxError;

/// The in-memory [`VmHost`], for this crate's tests and, with the
/// `test-support` feature, a dependent's.
#[cfg(any(test, feature = "test-support"))]
pub mod fake;
#[cfg(test)]
mod tests;

/// How long a guest gets to power off cleanly before it is killed.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the executor gets to finish its calls at shutdown.
pub const AGENT_GRACE: Duration = Duration::from_secs(2);

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
    /// Environment every command in the guest gets, on top of the git
    /// config the mounts need ([`MountPlan::git_env`]): the session's own
    /// identity (`AGENTIUM_SESSION`), as a host session's shell has it.
    /// Never a credential: whatever is here, the agent can print.
    pub env: &'a BTreeMap<String, String>,
    /// Virtual CPUs.
    pub cpus: u8,
    /// Memory, in MiB.
    pub memory_mib: u32,
}

/// What a [`VmHost`] has on record for one VM, running or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmRecord {
    /// The VM's name.
    pub name: String,
    /// Its labels.
    pub labels: BTreeMap<String, String>,
    /// Whether it is still up (anything but stopped or crashed).
    pub running: bool,
}

impl VmRecord {
    /// The record as [`verdict`] reads it.
    fn as_guest(&self) -> GuestRecord<'_> {
        GuestRecord {
            name: &self.name,
            labels: &self.labels,
            running: self.running,
        }
    }
}

/// One page of [`VmHost::list_ours`].
#[derive(Debug, Clone, Default)]
pub struct VmPage {
    /// The records on this page.
    pub records: Vec<VmRecord>,
    /// Where the next page starts, or `None` if this was the last.
    pub next: Option<String>,
}

/// What runs guests: microsandbox in a real build, an in-memory fake in
/// this crate's tests. Each method is one call to the host; the order they
/// are called in, and what a failure tears down, is [`Guests`]'s.
///
/// The futures are `Send` so a dropped [`Guest`] can spawn its own
/// teardown.
pub trait VmHost: Clone + Send + Sync + 'static {
    /// A live VM, as the create gave it back.
    type Vm: Send + Sync + 'static;

    /// The record named `name`, or `None` if there isn't one.
    fn get(
        &self,
        name: &str,
    ) -> impl Future<Output = Result<Option<VmRecord>, SandboxError>> + Send;

    /// One page of the records labelled as ours
    /// ([`crate::lifecycle::LABEL_APP`]), starting at `cursor` (`None`
    /// for the first page).
    fn list_ours(
        &self,
        cursor: Option<String>,
    ) -> impl Future<Output = Result<VmPage, SandboxError>> + Send;

    /// Create and boot the VM `name` with `labels`, set up from `spec`
    /// (mounts, image and agent, the route to notedeck, egress). Never
    /// replaces an existing record of that name.
    fn create(
        &self,
        name: &str,
        labels: &[GuestLabel],
        spec: &GuestSpec<'_>,
    ) -> impl Future<Output = Result<Self::Vm, SandboxError>> + Send;

    /// Check the created `vm` works (its DNS under the egress policy) and
    /// start its executor, giving the session with it.
    fn connect(
        &self,
        vm: &Self::Vm,
        spec: &GuestSpec<'_>,
    ) -> impl Future<Output = Result<GuestClient, SandboxError>> + Send;

    /// Stop the live `vm`, killing it if a clean stop takes longer than
    /// `timeout`.
    fn stop(
        &self,
        vm: &Self::Vm,
        timeout: Duration,
    ) -> impl Future<Output = Result<(), SandboxError>> + Send;

    /// Stop the VM recorded as `name`, as [`VmHost::stop`] does. Already
    /// gone is success.
    fn stop_named(
        &self,
        name: &str,
        timeout: Duration,
    ) -> impl Future<Output = Result<(), SandboxError>> + Send;

    /// Remove the record `name`. Already gone is success: another
    /// notedeck's sweep may have got there first.
    fn remove(&self, name: &str) -> impl Future<Output = Result<(), SandboxError>> + Send;

    /// Whether a process with this pid exists: whether the notedeck named
    /// in a guest's owner label is still alive.
    fn owner_alive(&self, pid: u32) -> bool;
}

/// The guests one notedeck runs: the bound on how many, who owns them,
/// and the host they run on.
#[derive(Debug, Clone)]
pub struct Guests<H: VmHost> {
    host: H,
    capacity: Capacity,
    owner_pid: u32,
}

/// A booted guest with a session open to its executor. Shut it down with
/// [`Guest::shutdown`]; dropping it reaps it in the background instead.
pub struct Guest<H: VmHost> {
    host: H,
    name: String,
    session_id: String,
    /// `None` only during teardown.
    vm: Option<H::Vm>,
    /// `None` until the executor is up, and during teardown.
    client: Option<GuestClient>,
    /// Given back when the guest is gone, not when this is dropped.
    slot: Option<Slot>,
}

impl<H: VmHost> Guests<H> {
    /// At most `max` guests at once ([`crate::lifecycle::DEFAULT_MAX_GUESTS`]
    /// is the usual) on `host`, owned by this process.
    pub fn with_host(host: H, max: usize) -> Self {
        Self {
            host,
            capacity: Capacity::new(max),
            owner_pid: std::process::id(),
        }
    }

    /// The host the guests run on.
    pub fn host(&self) -> &H {
        &self.host
    }

    /// The bound, and how much of it is in use.
    pub fn capacity(&self) -> &Capacity {
        &self.capacity
    }

    /// Boot a guest for `spec.session_id` and open a session with its
    /// executor, in the one order that works: create (mounts, image and
    /// agent, the route to notedeck if any, egress), then check DNS under
    /// the egress policy and start the executor ([`VmHost::connect`]). A
    /// failure after the create tears the guest down before returning.
    ///
    /// Refused with [`SandboxError::AtCapacity`] when the bound is reached,
    /// [`SandboxError::GuestRunning`] when the session already has a live
    /// guest (here or in another notedeck), and
    /// [`SandboxError::GuestNameTaken`] when a VM that isn't ours has
    /// the name. A stale record of the session's guest (a crash, a stop
    /// without a remove) is reaped first.
    pub async fn start(&self, spec: GuestSpec<'_>) -> Result<Guest<H>, SandboxError> {
        let name = guest_name(spec.session_id)?;
        let slot = self.capacity.try_take()?;
        self.clear_stale(&name, spec.session_id).await?;

        // Neither replacing (it would tear down another notedeck's live
        // guest of the same session) nor detached (see crate::lifecycle).
        let labels = labels(spec.session_id, self.owner_pid);
        let vm = self.host.create(&name, &labels, &spec).await?;

        let mut guest = Guest {
            host: self.host.clone(),
            name,
            session_id: spec.session_id.to_owned(),
            vm: Some(vm),
            client: None,
            slot: Some(slot),
        };
        match self.host.connect(guest.vm(), &spec).await {
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
    /// Errs only if the host can't list its VMs at all (no runtime); a
    /// guest that can't be reaped is in [`SweepReport::failed`].
    pub async fn sweep(&self) -> Result<SweepReport, SandboxError> {
        let mut report = SweepReport::default();
        let mut cursor = None;
        loop {
            let page = self.host.list_ours(cursor.take()).await?;
            for record in &page.records {
                match self.verdict(record) {
                    Verdict::NotOurs => {}
                    Verdict::Keep(_) => report.kept.push(record.name.clone()),
                    Verdict::Reap => match reap(&self.host, &record.name).await {
                        Ok(()) => report.reaped.push(record.name.clone()),
                        Err(e) => report.failed.push(SweepFailure {
                            name: record.name.clone(),
                            error: e.to_string(),
                        }),
                    },
                }
            }
            match page.next {
                Some(next) => cursor = Some(next),
                None => return Ok(report),
            }
        }
    }

    /// Make room for `name`: nothing there, or a record of ours to reap.
    async fn clear_stale(&self, name: &str, session_id: &str) -> Result<(), SandboxError> {
        let Some(record) = self.host.get(name).await? else {
            return Ok(());
        };
        match self.verdict(&record) {
            Verdict::NotOurs => Err(SandboxError::GuestNameTaken(name.to_owned())),
            Verdict::Keep(_) => Err(SandboxError::GuestRunning(session_id.to_owned())),
            Verdict::Reap => reap(&self.host, name).await,
        }
    }

    /// [`verdict`] on `record`, as this process, asking the host whether
    /// another owner lives.
    fn verdict(&self, record: &VmRecord) -> Verdict {
        verdict(&record.as_guest(), self.owner_pid, |pid| {
            self.host.owner_alive(pid)
        })
    }
}

impl<H: VmHost + Default> Default for Guests<H> {
    fn default() -> Self {
        Self::with_host(H::default(), crate::lifecycle::DEFAULT_MAX_GUESTS)
    }
}

impl<H: VmHost> Guest<H> {
    /// The VM's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The dave session this guest serves.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The live VM, for anything beyond tool calls.
    pub fn vm(&self) -> &H::Vm {
        self.vm.as_ref().expect("a guest has its VM until teardown")
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
        let vm = self.vm.take();
        let slot = self.slot.take();
        let result = teardown(&self.host, &self.name, vm).await;
        drop(slot);
        result
    }
}

impl<H: VmHost> Drop for Guest<H> {
    /// Reap a guest nobody shut down, in the background. Its slot goes with
    /// the task, so it still counts until it's gone.
    fn drop(&mut self) {
        let Some(vm) = self.vm.take() else {
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
        let host = self.host.clone();
        runtime.spawn(async move {
            drop(client);
            if let Err(e) = teardown(&host, &name, Some(vm)).await {
                eprintln!("agentium-sandbox: reaping dropped guest {name}: {e}");
            }
            drop(slot);
        });
    }
}

/// Stop the live `vm` if we have it, then reap `name` by name, so a stop
/// that failed still gets a kill and the record still goes.
async fn teardown<H: VmHost>(host: &H, name: &str, vm: Option<H::Vm>) -> Result<(), SandboxError> {
    if let Some(vm) = vm {
        let _ = host.stop(&vm, STOP_TIMEOUT).await;
    }
    reap(host, name).await
}

/// Stop the VM `name` on `host` if it's up (killing it if a clean stop
/// takes longer than [`STOP_TIMEOUT`]), then remove its record. Already
/// gone is success: another notedeck's sweep may have got there first.
pub async fn reap<H: VmHost>(host: &H, name: &str) -> Result<(), SandboxError> {
    let Some(record) = host.get(name).await? else {
        return Ok(());
    };
    if record.running {
        host.stop_named(name, STOP_TIMEOUT).await?;
    }
    host.remove(name).await
}
