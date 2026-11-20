//! Dave's microVM guests: the one [`Guests`] a sandboxed spec session's
//! coding tools will run in, and the sweep that reaps what a crashed
//! notedeck left behind.
//!
//! ```text
//! Dave::update (host, first frame)
//!   Sandboxes::sweep_at_startup ─▶ GuestPool::sweep_once ─▶ tokio task
//!                                     ready()?  no runtime: log, done
//!                                     Guests::sweep()      reap orphans, log
//! ```
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

#[cfg(any(sandbox, test))]
use agentium_sandbox::fleet::{Guests, VmHost};
#[cfg(any(sandbox, test))]
use agentium_sandbox::lifecycle::SweepReport;
use agentium_sandbox::Availability;

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
}

/// Whether this notedeck can boot a guest now, and if not, why. Looks for
/// the runtime on disk, so it is for a spec's first turn or a task, not
/// every frame.
// A plain build only reads it in tests until a sandboxed spec's tools ask
// it (headway:dave/miracle-curtain-wrestle).
#[cfg_attr(not(sandbox), allow(dead_code))]
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

/// One [`Guests`] on `H`, made on first use, and whether its startup
/// sweep has run.
#[cfg(any(sandbox, test))]
pub(crate) struct GuestPool<H: VmHost> {
    /// What the guests run on.
    host: H,
    /// How many may run at once.
    max: usize,
    /// Made by [`Self::guests`] the first time something needs it.
    guests: Option<Guests<H>>,
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
            guests: None,
            swept: false,
        }
    }

    /// The guests, made now if this is the first ask.
    pub(crate) fn guests(&mut self) -> &Guests<H> {
        self.guests
            .get_or_insert_with(|| Guests::with_host(self.host.clone(), self.max))
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use agentium_sandbox::client::GuestClient;
    use agentium_sandbox::fleet::{GuestSpec, VmPage, VmRecord};
    use agentium_sandbox::lifecycle::{guest_name, labels, GuestLabel};
    use agentium_sandbox::SandboxError;

    use super::*;

    /// A pid no process has: the owner of an orphaned guest.
    const DEAD_PID: u32 = 4_000_001;
    /// Another notedeck that is still running.
    const LIVE_PID: u32 = 4_000_002;

    /// What the fake host has on record and how often it was listed.
    #[derive(Default)]
    struct State {
        records: BTreeMap<String, VmRecord>,
        lists: usize,
    }

    /// An in-memory [`VmHost`] that only knows what a sweep asks: records
    /// to list, stop and remove. agentium-sandbox's own fake is private to
    /// its tests.
    #[derive(Clone, Default)]
    struct FakeHost {
        state: Arc<Mutex<State>>,
        live: Arc<BTreeSet<u32>>,
    }

    impl FakeHost {
        /// A running guest of ours for `session`, booted by `owner`.
        fn with_guest(self, session: &str, owner: u32) -> Self {
            let name = guest_name(session).unwrap();
            let labels = labels(session, owner)
                .into_iter()
                .map(|GuestLabel { key, value }| (key.to_owned(), value))
                .collect();
            let record = VmRecord {
                name: name.clone(),
                labels,
                running: true,
            };
            self.state.lock().unwrap().records.insert(name, record);
            self
        }

        fn names(&self) -> Vec<String> {
            self.state.lock().unwrap().records.keys().cloned().collect()
        }

        fn lists(&self) -> usize {
            self.state.lock().unwrap().lists
        }
    }

    impl VmHost for FakeHost {
        type Vm = ();

        async fn get(&self, name: &str) -> Result<Option<VmRecord>, SandboxError> {
            Ok(self.state.lock().unwrap().records.get(name).cloned())
        }

        async fn list_ours(&self, _cursor: Option<String>) -> Result<VmPage, SandboxError> {
            let mut state = self.state.lock().unwrap();
            state.lists += 1;
            Ok(VmPage {
                records: state.records.values().cloned().collect(),
                next: None,
            })
        }

        async fn create(
            &self,
            name: &str,
            _labels: &[GuestLabel],
            _spec: &GuestSpec<'_>,
        ) -> Result<(), SandboxError> {
            Err(SandboxError::GuestNameTaken(name.to_owned()))
        }

        async fn connect(
            &self,
            _vm: &(),
            spec: &GuestSpec<'_>,
        ) -> Result<GuestClient, SandboxError> {
            Err(SandboxError::GuestRunning(spec.session_id.to_owned()))
        }

        async fn stop(&self, _vm: &(), _timeout: Duration) -> Result<(), SandboxError> {
            Ok(())
        }

        async fn stop_named(&self, name: &str, _timeout: Duration) -> Result<(), SandboxError> {
            if let Some(record) = self.state.lock().unwrap().records.get_mut(name) {
                record.running = false;
            }
            Ok(())
        }

        async fn remove(&self, name: &str) -> Result<(), SandboxError> {
            self.state.lock().unwrap().records.remove(name);
            Ok(())
        }

        fn owner_alive(&self, pid: u32) -> bool {
            self.live.contains(&pid)
        }
    }

    fn fake() -> FakeHost {
        FakeHost {
            live: Arc::new(BTreeSet::from([LIVE_PID])),
            ..Default::default()
        }
        .with_guest("orphan", DEAD_PID)
        .with_guest("other-notedecks", LIVE_PID)
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
        assert_eq!(host.names(), ["agentium-other-notedecks"]);

        assert!(pool.sweep_once(|| Ok(())).is_none(), "only once");
        assert_eq!(host.lists(), 1);
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
        assert_eq!(host.lists(), 0);
        assert_eq!(host.names().len(), 2);
    }

    /// The guests are made on first use, and the same ones after.
    #[test]
    fn guests_are_made_lazily() {
        let mut pool = GuestPool::new(fake(), 3);
        assert!(pool.guests.is_none());
        assert_eq!(pool.guests().capacity().max(), 3);
        assert!(pool.guests.is_some());
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
}
