//! Dave's microVM guests: the one [`Guests`] a sandboxed spec session's
//! coding tools run in, a guest per session, and the sweep that reaps what
//! a crashed notedeck left behind.
//!
//! ```text
//! Dave::update (host, first frame)
//!   Sandboxes::sweep_at_startup ─▶ GuestPool::sweep_once ─▶ tokio task
//!                                     ready()?  no runtime: log, done
//!                                     Guests::sweep()      reap orphans, log
//!
//! dispatch_spec_turn (frame)       spec's coding runner is Microsandbox
//!   Sandboxes::session_sandbox ─▶ SessionSandbox on the session  (nothing booted)
//!
//! coding call (job thread)
//!   GuestCalls::run ─▶ SessionGuest: first call boots ─▶ Guests::start  (notedeck's runtime)
//!                                    later calls     ─▶ the same Guest
//!                    ─▶ guest.client().call(CodingCall) ─▶ answer
//!
//! session dropped (deleted, spec changed, notedeck exits)
//!   last Arc of SessionGuest ─▶ Guest::shutdown on notedeck's runtime
//! ```
//!
//! ## One guest per session
//!
//! A session's [`SessionSandbox`] is made on its spec turn's dispatch and
//! boots nothing: the guest is booted by the first coding call, as a job
//! off the frame (~150ms+ on KVM), in a [`tokio::sync::OnceCell`], so
//! concurrent first calls (a model's parallel Reads) share one boot. The
//! boot and the guest's protocol pump run on notedeck's own runtime, not the
//! job's short-lived one, so the guest outlives the call that booted it.
//!
//! The guest goes when the session lets go of it: a deleted session drops
//! its sandbox, as does one whose spec no longer runs coding in a guest,
//! and notedeck's exit drops every session. The last holder's drop shuts
//! the guest down on notedeck's runtime. A crash leaves it for the startup
//! sweep.
//!
//! **Desktop only.** The guests exist only in a build with the `sandbox`
//! feature on a platform microsandbox runs on, which `build.rs` folds into
//! `cfg(sandbox)`. Every other build (a plain desktop build, android, iOS)
//! has no pool, sweeps nothing, and [`ready`] says why it can't run a guest.
//!
//! The lifecycle itself (the order a guest boots in, when one may be
//! reaped, the bound on how many run) is `agentium_sandbox::fleet`'s. What is
//! here is when Dave runs it: lazily, once, off the frame. [`GuestPool`] is
//! generic over the [`VmHost`] so its tests run over an in-memory host
//! without KVM.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(any(sandbox, test))]
use std::sync::OnceLock;

use agentium_core::agent_spec::coding::CodingCall;
use agentium_core::agent_spec::SandboxGit;
#[cfg(any(sandbox, test))]
use agentium_sandbox::egress::Egress;
#[cfg(any(sandbox, test))]
use agentium_sandbox::fleet::{Guest, GuestSpec, Guests, VmHost};
#[cfg(any(sandbox, test))]
use agentium_sandbox::image::GuestSetup;
#[cfg(any(sandbox, test))]
use agentium_sandbox::lifecycle::SweepReport;
use agentium_sandbox::mounts::GitAccess;
#[cfg(any(sandbox, test))]
use agentium_sandbox::mounts::MountPlan;
use agentium_sandbox::Availability;
#[cfg(any(sandbox, test))]
use agentium_sandbox::SandboxError;

use crate::tool_round::ToolCancel;

/// Virtual CPUs a session's guest gets: enough for a build to use two
/// cores, few enough that [`agentium_sandbox::lifecycle::DEFAULT_MAX_GUESTS`]
/// of them fit beside notedeck.
#[cfg(any(sandbox, test))]
const GUEST_CPUS: u8 = 2;

/// Memory a session's guest gets, in MiB: a `cargo check` of a mid-sized
/// crate fits.
#[cfg(any(sandbox, test))]
const GUEST_MEMORY_MIB: u32 = 2048;

/// Why this notedeck can't run a sandboxed session's tools in a guest, for
/// a sandboxed spec to refuse with instead of a generic error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Unavailable {
    /// Built without Dave's `sandbox` feature.
    #[error("this notedeck was built without microVM sandboxes (the `sandbox` feature is off)")]
    FeatureOff,
    /// microsandbox doesn't run here.
    #[error("microVM sandboxes need KVM Linux or Apple-Silicon macOS")]
    UnsupportedPlatform,
    /// The build can run guests, but the host has no microsandbox runtime
    /// (`msb` and `libkrunfw`). Only a `cfg(sandbox)` build looks.
    #[cfg_attr(not(sandbox), allow(dead_code))]
    #[error("the microVM runtime isn't installed: {0}")]
    RuntimeMissing(String),
    /// There is no tokio runtime to boot a guest on (only a test without
    /// one gets here).
    #[cfg_attr(not(any(sandbox, test)), allow(dead_code))]
    #[error("no async runtime to boot a guest on")]
    NoRuntime,
}

/// The git profile a spec's `git` asks for, as the mounts spell it: a
/// readonly git dir to review, a writable one to commit.
#[cfg_attr(not(any(sandbox, test)), allow(dead_code))]
pub(crate) fn git_access(git: SandboxGit) -> GitAccess {
    match git {
        SandboxGit::ReadOnly => GitAccess::Review,
        SandboxGit::ReadWrite => GitAccess::Commit,
    }
}

/// What a sandboxed session's guest is booted for. A build without
/// guests refuses it unread.
#[cfg_attr(not(any(sandbox, test)), allow(dead_code))]
pub(crate) struct GuestRequest {
    /// The session's agentium id ([`event_session_id`]), which names the
    /// guest.
    ///
    /// [`event_session_id`]: crate::session::AgenticSessionData::event_session_id
    pub(crate) session_id: String,
    /// The session's working tree, mounted at the same path in the guest.
    pub(crate) checkout: PathBuf,
    /// What the guest may do to the git dir behind it.
    pub(crate) git: SandboxGit,
    /// The session's identity (`AGENTIUM_SESSION`, `AGENTIUM_SESSION_ID`),
    /// set for every command the guest runs, as the host runner's
    /// [`child_env`](crate::coding_tools::child_env) sets it for a Bash
    /// child. A guest has no host environment, so nothing is stripped.
    pub(crate) env: BTreeMap<String, String>,
}

/// A sandboxed session's guest, held on the session. Booted by its first
/// coding call; the last holder's drop shuts it down.
pub(crate) struct SessionSandbox {
    /// The git profile it was made for: a spec edited to the other one
    /// gets a new guest.
    pub(crate) git: SandboxGit,
    /// Runs the session's coding calls in the guest. A running call's job
    /// holds a clone, so a session deleted mid-call keeps its guest until
    /// that call ends.
    pub(crate) calls: Arc<dyn GuestCalls>,
}

/// A sandboxed session's coding calls, run in its guest. A trait so the
/// session holds one whatever runs the VMs: microsandbox, or a test's
/// in-memory host.
pub(crate) trait GuestCalls: Send + Sync {
    /// Run `call` in the session's guest, booting the guest first if this
    /// is its first call. Blocks the calling job thread until the guest
    /// answers or `cancel` (the round's stop) fires, which cancels the call
    /// in the guest (a running Bash is killed).
    fn run(&self, call: &CodingCall, cancel: &ToolCancel) -> Result<String, GuestCallError>;
}

/// Why a call run in a guest didn't answer. Only a guest makes one.
#[cfg_attr(not(any(sandbox, test)), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum GuestCallError {
    /// The round was stopped.
    #[error("cancelled")]
    Cancelled,
    /// The session's guest couldn't be booted: every slot taken, no guest
    /// binary, a failed boot. The turn shows it, since it isn't the tool's
    /// answer.
    #[error("this session's sandbox couldn't start: {0}")]
    Boot(String),
    /// The tool ran in the guest and failed (no such file, a timeout), or
    /// the guest went away: what the model reads.
    #[error("{0}")]
    Call(String),
}

/// Whether this notedeck can boot a guest now, and if not, why. Looks for
/// the runtime on disk, so it is for a spec's first turn or a task, not
/// every frame.
pub(crate) fn ready() -> Result<(), Unavailable> {
    if !cfg!(sandbox) {
        return Err(match agentium_sandbox::availability() {
            Availability::UnsupportedPlatform => Unavailable::UnsupportedPlatform,
            // A sandbox crate built with `microvm` through some other crate
            // still leaves Dave without guests: its own feature is off.
            Availability::FeatureOff | Availability::Built => Unavailable::FeatureOff,
        });
    }
    runtime_ready()
}

/// Whether microsandbox finds its runtime.
#[cfg(sandbox)]
fn runtime_ready() -> Result<(), Unavailable> {
    agentium_sandbox::runtime::resolve()
        .map(|_| ())
        .map_err(|e| Unavailable::RuntimeMissing(e.to_string()))
}

/// Only reached in a `cfg(sandbox)` build, where the other one is.
#[cfg(not(sandbox))]
fn runtime_ready() -> Result<(), Unavailable> {
    Ok(())
}

/// The guests this Dave runs, held on [`crate::Dave`] and passed down,
/// never global.
#[derive(Default)]
pub(crate) struct Sandboxes {
    /// microsandbox's guests; a build that can't run them has none.
    #[cfg(sandbox)]
    pool: GuestPool<agentium_sandbox::guests::Microsandbox>,
}

impl Sandboxes {
    /// Reap the guests a crashed notedeck left behind, once per process,
    /// on a tokio task. Called every host frame; after the first it costs a
    /// bool check. A build without guests does nothing.
    pub(crate) fn sweep_at_startup(&mut self) {
        #[cfg(sandbox)]
        self.pool.sweep_once(ready);
    }

    /// A guest for the session `request` describes, booted on its first
    /// coding call. Refused with why when this notedeck can't run guests
    /// ([`ready`]), so a sandboxed session never runs its calls anywhere
    /// else. Looks for the runtime on disk: once per session, not per frame.
    pub(crate) fn session_sandbox(
        &self,
        request: GuestRequest,
    ) -> Result<SessionSandbox, Unavailable> {
        ready()?;
        #[cfg(sandbox)]
        return self.pool.session_sandbox(request);
        // ready() refuses every build without guests.
        #[cfg(not(sandbox))]
        {
            drop(request);
            Err(Unavailable::FeatureOff)
        }
    }
}

#[cfg(sandbox)]
impl Default for GuestPool<agentium_sandbox::guests::Microsandbox> {
    fn default() -> Self {
        Self::new(
            agentium_sandbox::guests::Microsandbox,
            agentium_sandbox::lifecycle::DEFAULT_MAX_GUESTS,
        )
    }
}

/// Where a guest's image and `notedeck-agent` binary come from.
#[cfg(any(sandbox, test))]
type SetupSource = fn() -> Result<GuestSetup, SandboxError>;

/// This host's guest binary: a dev checkout's build (see
/// `agentium_sandbox::guest::locate`), on the default image.
#[cfg(any(sandbox, test))]
fn host_setup() -> Result<GuestSetup, SandboxError> {
    GuestSetup::for_host(None)
}

/// One [`Guests`] on `H`, made on first use, and whether its startup
/// sweep has run.
#[cfg(any(sandbox, test))]
pub(crate) struct GuestPool<H: VmHost> {
    /// What the guests run on.
    host: H,
    /// How many may run at once.
    max: usize,
    /// What a guest boots: [`host_setup`] unless a test says otherwise.
    setup: SetupSource,
    /// Made by [`Self::guests`] the first time something needs it.
    guests: OnceLock<Guests<H>>,
    /// Whether [`Self::sweep_once`] has started its sweep.
    swept: bool,
}

#[cfg(any(sandbox, test))]
impl<H: VmHost> GuestPool<H> {
    /// At most `max` guests at once on `host`; nothing is made yet.
    pub(crate) fn new(host: H, max: usize) -> Self {
        Self {
            host,
            max,
            setup: host_setup,
            guests: OnceLock::new(),
            swept: false,
        }
    }

    /// The same pool, its guests booted from what `setup` gives.
    #[cfg(test)]
    pub(crate) fn with_setup(mut self, setup: SetupSource) -> Self {
        self.setup = setup;
        self
    }

    /// The guests, made now if this is the first ask.
    pub(crate) fn guests(&self) -> &Guests<H> {
        self.guests
            .get_or_init(|| Guests::with_host(self.host.clone(), self.max))
    }

    /// A guest for `request`'s session, to boot on the current tokio
    /// runtime (notedeck's, on the frame) at its first call.
    pub(crate) fn session_sandbox(
        &self,
        request: GuestRequest,
    ) -> Result<SessionSandbox, Unavailable> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| Unavailable::NoRuntime)?;
        Ok(self.sandbox_on(request, runtime))
    }

    /// A guest for `request`'s session, to boot on `runtime` at its first
    /// call.
    pub(crate) fn sandbox_on(
        &self,
        request: GuestRequest,
        runtime: tokio::runtime::Handle,
    ) -> SessionSandbox {
        let git = request.git;
        let guest = SessionGuest {
            guests: self.guests().clone(),
            runtime,
            boot: Arc::new(GuestBoot {
                session_id: request.session_id,
                checkout: request.checkout,
                access: git_access(git),
                env: request.env,
                setup: self.setup,
            }),
            guest: tokio::sync::OnceCell::new(),
        };
        SessionSandbox {
            git,
            calls: Arc::new(guest),
        }
    }

    /// Start the startup sweep on the current tokio runtime, the first time
    /// only: later calls return `None`. The task checks `ready` first, so a
    /// host without the runtime touches nothing, and logs one line either
    /// way. Its result is the sweep's report, `None` when it didn't sweep.
    pub(crate) fn sweep_once(
        &mut self,
        ready: fn() -> Result<(), Unavailable>,
    ) -> Option<tokio::task::JoinHandle<Option<SweepReport>>> {
        if self.swept {
            return None;
        }
        self.swept = true;
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("sandbox: no tokio runtime, so no startup sweep of orphaned guests");
            return None;
        };
        let guests = self.guests().clone();
        Some(runtime.spawn(sweep(guests, ready)))
    }
}

/// What one session's guest is booted from, owned so the boot can run as
/// a task of its own.
#[cfg(any(sandbox, test))]
struct GuestBoot {
    session_id: String,
    checkout: PathBuf,
    access: GitAccess,
    env: BTreeMap<String, String>,
    setup: SetupSource,
}

#[cfg(any(sandbox, test))]
impl GuestBoot {
    /// Plan the mounts, find the guest binary and boot the guest on
    /// `guests`: the worktree read-write and the git dir per the profile,
    /// the default egress, and no route to notedeck (`ipc: None`), so the
    /// guest can't act as the user.
    async fn start<H: VmHost>(&self, guests: &Guests<H>) -> Result<Guest<H>, String> {
        let plan =
            MountPlan::for_checkout(&self.checkout, self.access).map_err(|e| e.to_string())?;
        let setup = (self.setup)().map_err(|e| e.to_string())?;
        let egress = Egress::coding();
        let spec = GuestSpec {
            session_id: &self.session_id,
            plan: &plan,
            setup: &setup,
            egress: &egress,
            ipc: None,
            env: &self.env,
            cpus: GUEST_CPUS,
            memory_mib: GUEST_MEMORY_MIB,
        };
        guests.start(spec).await.map_err(|e| e.to_string())
    }
}

/// One session's guest: booted by its first call, shut down when the last
/// holder drops it.
#[cfg(any(sandbox, test))]
struct SessionGuest<H: VmHost> {
    /// The pool's guests, which bound how many run.
    guests: Guests<H>,
    /// notedeck's runtime, which boots the guest, runs its protocol pump,
    /// and shuts it down.
    runtime: tokio::runtime::Handle,
    boot: Arc<GuestBoot>,
    /// Set by the first call to boot one; concurrent first calls wait on
    /// it rather than boot two. A failed boot leaves it empty for the next
    /// call to try again.
    guest: tokio::sync::OnceCell<Guest<H>>,
}

#[cfg(any(sandbox, test))]
impl<H: VmHost> SessionGuest<H> {
    /// The session's guest, booted now if no call has yet.
    async fn guest(&self) -> Result<&Guest<H>, GuestCallError> {
        self.guest
            .get_or_try_init(|| async {
                let guests = self.guests.clone();
                let boot = self.boot.clone();
                // On notedeck's runtime, not the job's: the guest's pump is
                // spawned where it connects, and must outlive this call.
                self.runtime
                    .spawn(async move { boot.start(&guests).await })
                    .await
                    .map_err(|e| GuestCallError::Boot(e.to_string()))?
                    .map_err(GuestCallError::Boot)
            })
            .await
    }

    /// Run `call` in the guest, booting it first if need be.
    async fn call(&self, call: &CodingCall) -> Result<String, GuestCallError> {
        let guest = self.guest().await?;
        let answer = guest.client().call(call.clone()).result().await;
        answer.map_err(|e| match e {
            agentium_sandbox::client::CallError::Cancelled => GuestCallError::Cancelled,
            e => GuestCallError::Call(e.to_string()),
        })
    }
}

#[cfg(any(sandbox, test))]
impl<H: VmHost> GuestCalls for SessionGuest<H> {
    fn run(&self, call: &CodingCall, cancel: &ToolCancel) -> Result<String, GuestCallError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| GuestCallError::Call(format!("no runtime for the tool: {e}")))?;
        runtime.block_on(async {
            tokio::select! {
                // Dropping the call on a stop cancels it in the guest
                // (`agentium_sandbox::client::Call`'s drop); a boot it
                // abandons finishes on notedeck's runtime and is reaped.
                _ = cancel.cancelled() => Err(GuestCallError::Cancelled),
                answer = self.call(call) => answer,
            }
        })
    }
}

#[cfg(any(sandbox, test))]
impl<H: VmHost> Drop for SessionGuest<H> {
    /// Shut the guest down on notedeck's runtime, if a call booted one.
    fn drop(&mut self) {
        let Some(guest) = self.guest.take() else {
            return;
        };
        self.runtime.spawn(async move {
            let name = guest.name().to_owned();
            if let Err(e) = guest.shutdown().await {
                tracing::warn!("sandbox: shutting down {name}: {e}");
            }
        });
    }
}

/// Sweep `guests` if `ready`, logging what it did.
#[cfg(any(sandbox, test))]
async fn sweep<H: VmHost>(
    guests: Guests<H>,
    ready: fn() -> Result<(), Unavailable>,
) -> Option<SweepReport> {
    if let Err(why) = ready() {
        tracing::info!("sandbox: no startup sweep: {why}");
        return None;
    }
    match guests.sweep().await {
        Ok(report) => {
            tracing::info!(
                "sandbox: startup sweep reaped {} orphaned guest(s), kept {}, failed {}",
                report.reaped.len(),
                report.kept.len(),
                report.failed.len()
            );
            for failure in &report.failed {
                tracing::warn!("sandbox: couldn't reap {}: {}", failure.name, failure.error);
            }
            Some(report)
        }
        Err(e) => {
            tracing::warn!("sandbox: startup sweep failed: {e}");
            None
        }
    }
}

/// Sandboxes over the in-memory host, for tests of the routing above
/// this module.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use agentium_sandbox::fleet::fake::FakeVms;

    /// A pool of at most `max` guests on `host`, booted from a placeholder
    /// binary the in-memory host never runs.
    pub(crate) fn pool(host: &FakeVms, max: usize) -> GuestPool<FakeVms> {
        GuestPool::new(host.clone(), max).with_setup(|| Ok(GuestSetup::new("/x/notedeck-agent")))
    }

    /// What session `session_id`'s guest boots for: `checkout`, readonly
    /// git, and its identity.
    pub(crate) fn request(session_id: &str, checkout: &std::path::Path) -> GuestRequest {
        GuestRequest {
            session_id: session_id.to_owned(),
            checkout: checkout.to_owned(),
            git: SandboxGit::ReadOnly,
            env: BTreeMap::from([(
                "AGENTIUM_SESSION".to_owned(),
                format!("agentium:{session_id}"),
            )]),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use agentium_core::agent_spec::coding::ReadInput;
    use agentium_sandbox::fleet::fake::{FakeVms, Op};
    use agentium_sandbox::fleet::VmRecord;
    use agentium_sandbox::lifecycle::{guest_name, labels, GuestLabel};

    use super::testing::{pool, request};
    use super::*;

    /// A pid no process has: the owner of an orphaned guest.
    const DEAD_PID: u32 = 4_000_001;
    /// Another notedeck that is still running.
    const LIVE_PID: u32 = 4_000_002;

    /// How long a guest's shutdown or a cancelled call may take.
    const PROMPTLY: Duration = Duration::from_secs(5);

    /// A running guest of ours for `session`, booted by `owner`.
    fn guest_record(session: &str, owner: u32) -> VmRecord {
        VmRecord {
            name: guest_name(session).unwrap(),
            labels: labels(session, owner)
                .into_iter()
                .map(|GuestLabel { key, value }| (key.to_owned(), value))
                .collect(),
            running: true,
        }
    }

    /// A host with an orphaned guest and a live notedeck's guest on file.
    fn fake() -> FakeVms {
        let host = FakeVms::default();
        host.set_alive(LIVE_PID);
        host.insert(guest_record("orphan", DEAD_PID));
        host.insert(guest_record("other-notedecks", LIVE_PID));
        host
    }

    fn lists(host: &FakeVms) -> usize {
        host.calls().iter().filter(|c| c.op == Op::List).count()
    }

    fn creates(host: &FakeVms, session: &str) -> usize {
        let name = guest_name(session).unwrap();
        host.ops_on(&name)
            .iter()
            .filter(|op| **op == Op::Create)
            .count()
    }

    /// The first call sweeps: the guest whose notedeck died is reaped, and
    /// a live notedeck's is kept. Every later call does nothing.
    #[tokio::test]
    async fn the_startup_sweep_reaps_orphans_once() {
        let host = fake();
        let mut pool = GuestPool::new(host.clone(), 4);

        let report = pool
            .sweep_once(|| Ok(()))
            .expect("the first call sweeps")
            .await
            .unwrap()
            .expect("ready, so it swept");
        assert_eq!(report.reaped, ["agentium-orphan"]);
        assert_eq!(report.kept, ["agentium-other-notedecks"]);
        assert!(host.record("agentium-orphan").is_none());
        assert!(host.record("agentium-other-notedecks").is_some());

        assert!(pool.sweep_once(|| Ok(())).is_none(), "only once");
        assert_eq!(lists(&host), 1);
    }

    /// A host without the runtime doesn't touch microsandbox at all.
    #[tokio::test]
    async fn no_runtime_no_sweep() {
        let host = fake();
        let mut pool = GuestPool::new(host.clone(), 4);
        let swept = pool
            .sweep_once(|| Err(Unavailable::RuntimeMissing("no msb".into())))
            .expect("the task runs")
            .await
            .unwrap();
        assert!(swept.is_none());
        assert!(host.calls().is_empty());
    }

    /// The guests are made on first use, and the same ones after.
    #[test]
    fn guests_are_made_lazily() {
        let pool = GuestPool::new(fake(), 3);
        assert!(pool.guests.get().is_none());
        assert_eq!(pool.guests().capacity().max(), 3);
        assert!(pool.guests.get().is_some());
    }

    /// A build without the guests says why; one with them asks the runtime.
    #[test]
    fn ready_matches_the_build() {
        let ready = ready();
        let guests = cfg!(sandbox);
        let looked_for_runtime = matches!(ready, Ok(()) | Err(Unavailable::RuntimeMissing(_)));
        assert_eq!(looked_for_runtime, guests, "{ready:?}");
        let desktop = cfg!(any(
            target_os = "linux",
            all(target_os = "macos", target_arch = "aarch64")
        ));
        assert_eq!(
            ready == Err(Unavailable::UnsupportedPlatform),
            !desktop,
            "{ready:?}"
        );
    }

    /// Run `call` in `sandbox` as a job does: on a thread of its own,
    /// stopped by `cancel`.
    async fn run(
        sandbox: &SessionSandbox,
        call: CodingCall,
        cancel: &ToolCancel,
    ) -> Result<String, GuestCallError> {
        let calls = sandbox.calls.clone();
        let cancel = cancel.clone();
        tokio::task::spawn_blocking(move || calls.run(&call, &cancel))
            .await
            .unwrap()
    }

    fn read(path: &str) -> CodingCall {
        CodingCall::Read(ReadInput {
            file_path: path.into(),
            offset: None,
            limit: None,
        })
    }

    /// A Bash call; Unix only, like the guest's Bash.
    #[cfg(unix)]
    fn bash(command: &str) -> CodingCall {
        CodingCall::Bash(agentium_core::agent_spec::coding::BashInput {
            command: command.into(),
            description: None,
            timeout: None,
        })
    }

    /// Making a session's sandbox boots nothing. Its first call boots the
    /// guest; later calls reuse it. An Edit lands in the mounted worktree,
    /// and the guest's Bash has the session's identity.
    ///
    /// Unix only, like the guest's Bash, which runs `/bin/sh`.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn the_first_call_boots_the_guest_and_later_ones_reuse_it() {
        let tree = tempfile::tempdir().unwrap();
        std::fs::write(tree.path().join("a.txt"), "hello\n").unwrap();
        let host = FakeVms::default();
        let pool = pool(&host, 2);
        let sandbox = pool.session_sandbox(request("s1", tree.path())).unwrap();
        assert!(host.calls().is_empty(), "nothing boots until a call");
        let cancel = ToolCancel::default();

        let edit = CodingCall::Edit(agentium_core::agent_spec::coding::EditInput {
            file_path: "a.txt".into(),
            old_string: "hello".into(),
            new_string: "bye".into(),
            replace_all: false,
        });
        run(&sandbox, edit, &cancel).await.expect("the edit runs");
        assert_eq!(
            std::fs::read_to_string(tree.path().join("a.txt")).unwrap(),
            "bye\n"
        );
        let who = run(&sandbox, bash("echo $AGENTIUM_SESSION"), &cancel)
            .await
            .expect("bash runs");
        assert!(who.contains("agentium:s1"), "{who}");
        assert_eq!(creates(&host, "s1"), 1, "one boot for both calls");
        assert_eq!(pool.guests().capacity().in_use(), 1);
    }

    /// A model's parallel first calls share one boot rather than race two
    /// guests for the session's name.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_first_calls_boot_one_guest() {
        let tree = tempfile::tempdir().unwrap();
        std::fs::write(tree.path().join("a.txt"), "hello\n").unwrap();
        let host = FakeVms::default();
        let pool = pool(&host, 2);
        let sandbox = pool.session_sandbox(request("s1", tree.path())).unwrap();
        let cancel = ToolCancel::default();

        let reads = (0..4).map(|_| run(&sandbox, read("a.txt"), &cancel));
        for answer in futures::future::join_all(reads).await {
            assert!(answer.expect("every read runs").contains("hello"));
        }
        assert_eq!(creates(&host, "s1"), 1);
    }

    /// The session letting go of its sandbox (deleted, its spec changed,
    /// notedeck exiting) stops the guest, removes its record and frees its
    /// slot.
    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_the_sandbox_shuts_its_guest_down() {
        let tree = tempfile::tempdir().unwrap();
        std::fs::write(tree.path().join("a.txt"), "hello\n").unwrap();
        let host = FakeVms::default();
        let pool = pool(&host, 2);
        let sandbox = pool.session_sandbox(request("s1", tree.path())).unwrap();
        run(&sandbox, read("a.txt"), &ToolCancel::default())
            .await
            .unwrap();
        let name = guest_name("s1").unwrap();
        assert!(host.record(&name).is_some());

        drop(sandbox);
        let deadline = Instant::now() + PROMPTLY;
        while host.record(&name).is_some() || pool.guests().capacity().in_use() > 0 {
            assert!(Instant::now() < deadline, "the guest is still up");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let ops = host.ops_on(&name);
        assert!(ops.contains(&Op::Stop), "{ops:?}");
        assert_eq!(ops.last(), Some(&Op::Remove), "{ops:?}");
    }

    /// A sandbox that never ran a call has no guest to stop.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_sandbox_never_used_touches_nothing() {
        let tree = tempfile::tempdir().unwrap();
        let host = FakeVms::default();
        let pool = pool(&host, 2);
        drop(pool.session_sandbox(request("s1", tree.path())).unwrap());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(host.calls().is_empty());
    }

    /// With every slot taken, another session's first call is a boot error
    /// that says so, not a tool's answer.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_full_pool_is_a_boot_error() {
        let tree = tempfile::tempdir().unwrap();
        std::fs::write(tree.path().join("a.txt"), "hello\n").unwrap();
        let host = FakeVms::default();
        let pool = pool(&host, 1);
        let cancel = ToolCancel::default();
        let first = pool.session_sandbox(request("s1", tree.path())).unwrap();
        run(&first, read("a.txt"), &cancel).await.unwrap();

        let second = pool.session_sandbox(request("s2", tree.path())).unwrap();
        let err = run(&second, read("a.txt"), &cancel)
            .await
            .expect_err("no room");
        let GuestCallError::Boot(why) = &err else {
            panic!("a boot error: {err:?}");
        };
        assert!(why.contains("1"), "names the bound: {why}");
        assert!(
            err.to_string()
                .starts_with("this session's sandbox couldn't start"),
            "{err}"
        );
        assert_eq!(creates(&host, "s2"), 0);
    }

    /// The round's stop cancels a running Bash in the guest, promptly.
    ///
    /// Unix only, like the guest's Bash, which runs `/bin/sh`.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stop_cancels_a_running_call() {
        let tree = tempfile::tempdir().unwrap();
        let host = FakeVms::default();
        let pool = pool(&host, 1);
        let sandbox = pool.session_sandbox(request("s1", tree.path())).unwrap();
        let cancel = ToolCancel::default();
        run(&sandbox, bash("true"), &cancel).await.unwrap();

        let started = Instant::now();
        let stopper = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            stopper.cancel();
        });
        let ran = run(&sandbox, bash("sleep 30"), &cancel).await;
        assert_eq!(ran, Err(GuestCallError::Cancelled));
        assert!(started.elapsed() < PROMPTLY, "{:?}", started.elapsed());
    }

    /// A tool that fails in the guest answers why, for the model.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failing_tool_answers_its_error() {
        let tree = tempfile::tempdir().unwrap();
        let host = FakeVms::default();
        let pool = pool(&host, 1);
        let sandbox = pool.session_sandbox(request("s1", tree.path())).unwrap();
        let err = run(&sandbox, read("missing.txt"), &ToolCancel::default())
            .await
            .expect_err("no such file");
        assert!(matches!(err, GuestCallError::Call(_)), "{err:?}");
    }
}
