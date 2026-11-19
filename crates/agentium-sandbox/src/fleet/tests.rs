//! The lifecycle over [`FakeVms`]: start, shutdown, the reap of a dropped
//! guest, the stale check and the sweep, with no KVM. `tests/boot.rs` runs
//! the same paths on real hardware.

use std::collections::BTreeMap;
use std::time::Duration;

use notedeck_agent_proto::call::{CodingCall, ReadInput};

use super::fake::{FakeVms, Op};
use super::{GuestSpec, Guests, VmRecord};
use crate::egress::Egress;
use crate::image::GuestSetup;
use crate::lifecycle::{guest_name, labels, LABEL_OWNER_PID, LABEL_SESSION};
use crate::mounts::{GitAccess, MountPlan};
use crate::SandboxError;

/// A pid the fake reports as dead.
const DEAD: u32 = u32::MAX - 1;
/// A pid the fake reports as another live notedeck.
const ALIVE: u32 = u32::MAX - 2;

/// How long a dropped guest's background reap may take.
const PROMPTLY: Duration = Duration::from_secs(5);

/// What a guest boots from: a temp dir that isn't a repo, as its checkout.
struct Fixture {
    root: tempfile::TempDir,
    plan: MountPlan,
    setup: GuestSetup,
    egress: Egress,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let plan = MountPlan::for_checkout(root.path(), GitAccess::Review).unwrap();
        Self {
            root,
            plan,
            setup: GuestSetup::new("/x/notedeck-agent"),
            egress: Egress::offline(),
        }
    }

    fn spec<'a>(&'a self, session_id: &'a str) -> GuestSpec<'a> {
        GuestSpec {
            session_id,
            plan: &self.plan,
            setup: &self.setup,
            egress: &self.egress,
            ipc: None,
            cpus: 1,
            memory_mib: 256,
        }
    }
}

/// A host with another live notedeck on it, and `max` guests for us.
fn guests(max: usize) -> Guests<FakeVms> {
    let host = FakeVms::default();
    host.set_alive(ALIVE);
    Guests::with_host(host, max)
}

/// A record of ours for `session_id`, booted by `owner` (no owner label
/// for `None`).
fn ours(session_id: &str, owner: Option<u32>, running: bool) -> VmRecord {
    let mut labels: BTreeMap<String, String> = labels(session_id, owner.unwrap_or(0))
        .into_iter()
        .map(|l| (l.key.to_owned(), l.value))
        .collect();
    if owner.is_none() {
        labels.remove(LABEL_OWNER_PID);
    }
    VmRecord {
        name: guest_name(session_id).unwrap(),
        labels,
        running,
    }
}

/// A user's own VM, with no app label, named like a guest of ours.
fn theirs(session_id: &str) -> VmRecord {
    VmRecord {
        name: guest_name(session_id).unwrap(),
        labels: BTreeMap::new(),
        running: true,
    }
}

/// A started guest is labelled for its session and this process, holds a
/// slot, and runs tool calls; shutdown stops it, removes its record and
/// frees the slot.
#[tokio::test]
async fn start_then_shutdown_leaves_nothing_behind() {
    let fx = Fixture::new();
    std::fs::write(fx.root.path().join("hello.txt"), "hi from the guest\n").unwrap();
    let guests = guests(2);

    let guest = guests.start(fx.spec("s1")).await.expect("start");
    let name = guest.name().to_owned();
    let record = guests.host().record(&name).expect("recorded");
    assert!(record.running);
    assert_eq!(record.labels.get(LABEL_SESSION).unwrap(), "s1");
    assert_eq!(
        record.labels.get(LABEL_OWNER_PID).unwrap(),
        &std::process::id().to_string()
    );
    assert_eq!(guests.capacity().in_use(), 1);

    let read = guest
        .client()
        .call(CodingCall::Read(ReadInput {
            file_path: fx.root.path().join("hello.txt").to_string_lossy().into(),
            offset: None,
            limit: None,
        }))
        .result()
        .await
        .expect("a call reaches the guest");
    assert!(read.contains("hi from the guest"), "{read}");

    guest.shutdown().await.expect("shutdown");
    assert_eq!(guests.capacity().in_use(), 0);
    assert!(guests.host().record(&name).is_none(), "record left behind");
    assert_eq!(
        guests.host().ops_on(&name),
        [
            Op::Get,
            Op::Create,
            Op::Connect,
            Op::Stop,
            Op::Get,
            Op::Remove
        ]
    );
}

/// A boot that fails once the VM exists (DNS, the executor) tears it
/// down before reporting the boot's own error.
#[tokio::test]
async fn a_start_that_fails_after_create_tears_the_guest_down() {
    let fx = Fixture::new();
    let guests = guests(1);
    guests.host().fail(Op::Connect);

    let err = guests.start(fx.spec("s1")).await.err().expect("refused");
    assert!(
        matches!(err, SandboxError::Fake(ref m) if m.starts_with("Connect")),
        "{err}"
    );

    let name = guest_name("s1").unwrap();
    assert!(guests.host().record(&name).is_none(), "record left behind");
    assert_eq!(guests.capacity().in_use(), 0);
    let ops = guests.host().ops_on(&name);
    assert_eq!(&ops[ops.len() - 2..], [Op::Get, Op::Remove], "{ops:?}");
    assert!(ops.contains(&Op::Stop), "{ops:?}");
}

/// A create that fails leaves no record to tear down, and gives the slot
/// back.
#[tokio::test]
async fn a_failed_create_frees_its_slot() {
    let fx = Fixture::new();
    let guests = guests(1);
    guests.host().fail(Op::Create);

    guests.start(fx.spec("s1")).await.err().expect("refused");
    assert_eq!(guests.capacity().in_use(), 0);
    assert_eq!(
        guests.host().ops_on(&guest_name("s1").unwrap()),
        [Op::Get, Op::Create]
    );
}

/// At the bound a start is refused at once, before the host is asked
/// anything.
#[tokio::test]
async fn a_full_bound_refuses_without_touching_the_host() {
    let fx = Fixture::new();
    let guests = guests(1);
    let first = guests.start(fx.spec("s1")).await.expect("first");

    let err = guests.start(fx.spec("s2")).await.err().expect("refused");
    assert!(matches!(err, SandboxError::AtCapacity { max: 1 }), "{err}");
    assert!(guests.host().ops_on(&guest_name("s2").unwrap()).is_empty());

    first.shutdown().await.expect("shutdown");
    guests.start(fx.spec("s2")).await.expect("room again");
}

/// A guest dropped without a shutdown (an aborted task) is stopped and
/// removed in the background, and holds its slot until that is done.
#[tokio::test]
async fn a_dropped_guest_is_reaped_in_the_background() {
    let fx = Fixture::new();
    let guests = guests(1);
    let guest = guests.start(fx.spec("s1")).await.expect("start");
    let name = guest.name().to_owned();

    drop(guest);
    // A current-thread runtime: the reap hasn't run yet.
    assert_eq!(guests.capacity().in_use(), 1, "freed before it was gone");

    tokio::time::timeout(PROMPTLY, async {
        while guests.capacity().in_use() > 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the dropped guest's slot comes back");
    assert!(guests.host().record(&name).is_none(), "record left behind");
    assert!(guests
        .host()
        .ops_on(&name)
        .ends_with(&[Op::Stop, Op::Get, Op::Remove]));
}

/// The stale check before a create: a live guest of the session (ours or
/// another notedeck's) refuses, a stranger's VM with the name refuses, and
/// a stopped record of ours is reaped to make room.
#[tokio::test]
async fn a_start_clears_only_what_is_stale() {
    let fx = Fixture::new();
    let guests = guests(4);
    let host = guests.host();
    host.insert(ours("mine", Some(std::process::id()), true));
    host.insert(ours("other", Some(ALIVE), true));
    host.insert(theirs("taken"));
    host.insert(ours("stale", Some(ALIVE), false));
    host.insert(ours("orphan", Some(DEAD), true));

    for session in ["mine", "other"] {
        let err = guests.start(fx.spec(session)).await.err().expect(session);
        assert!(
            matches!(err, SandboxError::GuestRunning(ref s) if s == session),
            "{err}"
        );
    }
    let err = guests.start(fx.spec("taken")).await.err().expect("taken");
    assert!(matches!(err, SandboxError::GuestNameTaken(_)), "{err}");
    for session in ["mine", "other", "taken"] {
        let name = guest_name(session).unwrap();
        assert!(host.record(&name).unwrap().running, "{session} was touched");
    }
    assert_eq!(
        guests.capacity().in_use(),
        0,
        "a refused start kept its slot"
    );

    let stale = guests.start(fx.spec("stale")).await.expect("stale reaped");
    let orphan = guests
        .start(fx.spec("orphan"))
        .await
        .expect("orphan reaped");
    assert_eq!(
        host.ops_on(&guest_name("stale").unwrap())[..4],
        [Op::Get, Op::Get, Op::Remove, Op::Create]
    );
    assert_eq!(
        host.ops_on(&guest_name("orphan").unwrap())[..5],
        [Op::Get, Op::Get, Op::StopNamed, Op::Remove, Op::Create]
    );
    stale.shutdown().await.unwrap();
    orphan.shutdown().await.unwrap();
}

/// The sweep pages through every guest of ours and reaps the orphans and
/// the stopped, keeping this process's running guests, another live
/// notedeck's, and one whose owner can't be read. A stranger's VM is never
/// listed, let alone touched.
#[tokio::test]
async fn a_sweep_pages_and_reaps_orphans_and_stopped_guests() {
    let guests = guests(1);
    let host = guests.host();
    host.insert(ours("a-orphan", Some(DEAD), true));
    host.insert(ours("b-stopped", Some(std::process::id()), false));
    host.insert(ours("c-live", Some(ALIVE), true));
    host.insert(ours("d-mine", Some(std::process::id()), true));
    host.insert(ours("e-unowned", None, true));
    host.insert(theirs("f-theirs"));

    let report = guests.sweep().await.expect("sweep");
    let names = |sessions: &[&str]| -> Vec<String> {
        sessions.iter().map(|s| guest_name(s).unwrap()).collect()
    };
    assert_eq!(report.reaped, names(&["a-orphan", "b-stopped"]));
    assert_eq!(report.kept, names(&["c-live", "d-mine", "e-unowned"]));
    assert!(report.failed.is_empty(), "{report:?}");

    for gone in names(&["a-orphan", "b-stopped"]) {
        assert!(host.record(&gone).is_none(), "{gone} left behind");
    }
    for left in names(&["c-live", "d-mine", "e-unowned", "f-theirs"]) {
        assert!(host.record(&left).unwrap().running, "{left} was touched");
    }
    assert!(host.ops_on(&guest_name("f-theirs").unwrap()).is_empty());
    let lists = host.calls().iter().filter(|c| c.op == Op::List).count();
    assert_eq!(lists, 3, "five of ours, two to a page");
}

/// A guest that can't be reaped is reported, and the sweep goes on to the
/// rest; a host that can't list at all fails the sweep.
#[tokio::test]
async fn a_sweep_reports_what_it_could_not_reap() {
    let guests = guests(1);
    let host = guests.host();
    host.insert(ours("a-orphan", Some(DEAD), true));
    host.insert(ours("b-stopped", Some(DEAD), false));
    let orphan = guest_name("a-orphan").unwrap();
    host.fail_on(Op::Remove, &orphan);

    let report = guests.sweep().await.expect("sweep");
    assert_eq!(report.reaped, [guest_name("b-stopped").unwrap()]);
    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert_eq!(report.failed[0].name, orphan);
    assert!(report.failed[0].error.contains("Remove"), "{report:?}");

    host.fail(Op::List);
    let err = guests.sweep().await.expect_err("no listing");
    assert!(matches!(err, SandboxError::Fake(_)), "{err}");
}
