//! `FakeVms`: an in-memory [`VmHost`] for the lifecycle tests. It keeps
//! records the way microsandbox does (a create refuses a name that has
//! one, a stop keeps it, a remove drops it), logs every call, pages its
//! listing, fails any call on request, and connects to the real guest loop
//! (`notedeck_agent_guest::serve`) over in-memory pipes, as
//! `tests/protocol.rs` does.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use notedeck_agent_guest::{serve, ChildEnv, Executor};
use notedeck_agent_proto::wire::Version;
use tokio::io::BufReader;

use super::{GuestSpec, VmHost, VmPage, VmRecord};
use crate::client::{connect, GuestClient, Pipe};
use crate::guest::HOST_BUILD;
use crate::lifecycle::{GuestLabel, APP, LABEL_APP};
use crate::SandboxError;

/// The listing's page size unless told otherwise: small, so a sweep over a
/// handful of records pages.
const PAGE: usize = 2;

/// One [`VmHost`] method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Get,
    List,
    Create,
    Connect,
    Stop,
    StopNamed,
    Remove,
}

/// One call the host took: the method, and the VM it named (the cursor
/// for a [`Op::List`], empty for the first page).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeCall {
    pub op: Op,
    pub name: String,
}

/// A VM the fake created.
#[derive(Debug)]
pub struct FakeVm {
    name: String,
}

/// A call to fail: every `op`, or only on the VM `name`.
#[derive(Debug)]
struct Failure {
    op: Op,
    name: Option<String>,
}

#[derive(Debug, Default)]
struct State {
    records: BTreeMap<String, VmRecord>,
    calls: Vec<FakeCall>,
    failing: Vec<Failure>,
    live_pids: BTreeSet<u32>,
}

/// The in-memory host. Clones share their state, as clones of a real host
/// share one runtime.
#[derive(Debug, Clone, Default)]
pub struct FakeVms {
    state: Arc<Mutex<State>>,
}

impl FakeVms {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("fake host state")
    }

    /// Put `record` on file, as a VM some earlier run left behind.
    pub fn insert(&self, record: VmRecord) {
        self.state().records.insert(record.name.clone(), record);
    }

    /// The record `name`, if there is one.
    pub fn record(&self, name: &str) -> Option<VmRecord> {
        self.state().records.get(name).cloned()
    }

    /// Every call so far, in order.
    pub fn calls(&self) -> Vec<FakeCall> {
        self.state().calls.clone()
    }

    /// The calls that named `name`, in order.
    pub fn ops_on(&self, name: &str) -> Vec<Op> {
        self.state()
            .calls
            .iter()
            .filter(|c| c.name == name && c.op != Op::List)
            .map(|c| c.op)
            .collect()
    }

    /// Fail every call of `op` from now on.
    pub fn fail(&self, op: Op) {
        self.state().failing.push(Failure { op, name: None });
    }

    /// Fail every call of `op` on the VM `name` from now on.
    pub fn fail_on(&self, op: Op, name: &str) {
        self.state().failing.push(Failure {
            op,
            name: Some(name.to_owned()),
        });
    }

    /// Report `pid` as a live process.
    pub fn set_alive(&self, pid: u32) {
        self.state().live_pids.insert(pid);
    }

    /// Log the call, and fail it if told to.
    fn enter(&self, op: Op, name: &str) -> Result<(), SandboxError> {
        let mut state = self.state();
        state.calls.push(FakeCall {
            op,
            name: name.to_owned(),
        });
        let fails = state
            .failing
            .iter()
            .any(|f| f.op == op && f.name.as_deref().is_none_or(|n| n == name));
        if fails {
            return Err(SandboxError::Fake(format!("{op:?} {name}")));
        }
        Ok(())
    }

    /// Mark the record `name` stopped, if there is one.
    fn mark_stopped(&self, name: &str) {
        if let Some(record) = self.state().records.get_mut(name) {
            record.running = false;
        }
    }
}

impl VmHost for FakeVms {
    type Vm = FakeVm;

    async fn get(&self, name: &str) -> Result<Option<VmRecord>, SandboxError> {
        self.enter(Op::Get, name)?;
        Ok(self.record(name))
    }

    async fn list_ours(&self, cursor: Option<String>) -> Result<VmPage, SandboxError> {
        let after = cursor.unwrap_or_default();
        self.enter(Op::List, &after)?;
        let state = self.state();
        let mut ours = state
            .records
            .values()
            .filter(|r| r.labels.get(LABEL_APP).map(String::as_str) == Some(APP))
            .filter(|r| r.name > after);
        let records: Vec<VmRecord> = ours.by_ref().take(PAGE).cloned().collect();
        let next = match ours.next() {
            Some(_) => records.last().map(|r| r.name.clone()),
            None => None,
        };
        Ok(VmPage { records, next })
    }

    async fn create(
        &self,
        name: &str,
        labels: &[GuestLabel],
        _spec: &GuestSpec<'_>,
    ) -> Result<FakeVm, SandboxError> {
        self.enter(Op::Create, name)?;
        let mut state = self.state();
        if state.records.contains_key(name) {
            return Err(SandboxError::Fake(format!("{name} already exists")));
        }
        let record = VmRecord {
            name: name.to_owned(),
            labels: labels
                .iter()
                .map(|l| (l.key.to_owned(), l.value.clone()))
                .collect(),
            running: true,
        };
        state.records.insert(name.to_owned(), record);
        Ok(FakeVm {
            name: name.to_owned(),
        })
    }

    async fn connect(
        &self,
        vm: &FakeVm,
        spec: &GuestSpec<'_>,
    ) -> Result<GuestClient, SandboxError> {
        self.enter(Op::Connect, &vm.name)?;
        let (host_out, guest_in) = tokio::io::duplex(1 << 16);
        let (guest_out, host_in) = tokio::io::duplex(1 << 16);
        tokio::spawn(serve(
            // The guest's executor inherits the VM's environment, which is
            // what a real create sets `spec.env` on.
            Executor::new(spec.plan.checkout.clone()).with_env(ChildEnv {
                set: spec.env.clone(),
                remove: Vec::new(),
            }),
            Version::new(HOST_BUILD),
            BufReader::new(guest_in),
            guest_out,
        ));
        Ok(connect(Pipe(host_in), Pipe(host_out), HOST_BUILD).await?)
    }

    async fn stop(&self, vm: &FakeVm, _timeout: Duration) -> Result<(), SandboxError> {
        self.enter(Op::Stop, &vm.name)?;
        self.mark_stopped(&vm.name);
        Ok(())
    }

    async fn stop_named(&self, name: &str, _timeout: Duration) -> Result<(), SandboxError> {
        self.enter(Op::StopNamed, name)?;
        self.mark_stopped(name);
        Ok(())
    }

    async fn remove(&self, name: &str) -> Result<(), SandboxError> {
        self.enter(Op::Remove, name)?;
        self.state().records.remove(name);
        Ok(())
    }

    fn owner_alive(&self, pid: u32) -> bool {
        self.state().live_pids.contains(&pid)
    }
}
