//! Who owns a guest, and when one may be reaped: the rules behind
//! [`crate::fleet`], kept apart from any VM host so they are tested in
//! every build.
//!
//! A guest is a microVM, and a microVM nobody stops is a leak with the
//! user's checkout mounted in it. Three things keep that from happening:
//!
//! - **Attached, never detached.** A guest is created attached to the
//!   notedeck that booted it: microsandbox ties it to its owner through a
//!   watchdog pipe, so when notedeck exits, even by `SIGKILL`, the VM shuts
//!   down. Detached mode would survive a restart, but a guest is stateless
//!   (the conversation lives on the host), so rebooting one (147 ms) is
//!   always cheaper than the leak risk of keeping one.
//! - **Stop is not enough; remove.** A stopped sandbox keeps its record,
//!   and a record makes the next create of that name fail. Every teardown
//!   is stop then [remove](microsandbox::Sandbox::remove).
//! - **A sweep for what a crash left behind.** Attached mode kills the VM
//!   on a crash but cannot remove its record. [`crate::fleet::Guests::sweep`]
//!   finds every guest labelled as ours ([`LABEL_APP`]) and reaps those
//!   whose owner is gone, by the [`verdict`] below.
//!
//! **Identity.** A guest is named [`NAME_PREFIX`]`<session id>` ([`guest_name`]),
//! one guest per dave session, and carries three labels: [`LABEL_APP`]
//! (ours at all), [`LABEL_SESSION`] (which session) and [`LABEL_OWNER_PID`]
//! (which notedeck process). The name alone is never proof: a user's own
//! sandbox may be called anything, so a sandbox without our app label is
//! never touched. The owner pid is what keeps one notedeck's sweep off
//! another live notedeck's guests on the same machine (several share
//! `~/.microsandbox`).
//!
//! **The bound.** At most [`DEFAULT_MAX_GUESTS`] guests per notedeck
//! ([`Capacity`]). Starting one more is refused at once with
//! [`SandboxError::AtCapacity`], not queued: a session that silently waits
//! for another to end looks hung, and the caller can say "stop a sandboxed
//! session first" instead.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::SandboxError;

/// What every guest's name starts with. Cosmetic: [`LABEL_APP`] decides
/// what is ours, not the name.
pub const NAME_PREFIX: &str = "agentium-";

/// The longest session id a guest can be named after. With
/// [`NAME_PREFIX`] that stays well under microsandbox's 128-byte name cap,
/// and under its socket-path budget for a normal `$MSB_HOME`.
pub const MAX_SESSION_ID: usize = 64;

/// The label that marks a sandbox as one of ours, with the value
/// [`APP`]. The sweep lists by it, so it never sees a user's own.
pub const LABEL_APP: &str = "notedeck.app";

/// [`LABEL_APP`]'s value.
pub const APP: &str = "agentium";

/// The label naming the dave session a guest serves.
pub const LABEL_SESSION: &str = "notedeck.session";

/// The label naming the pid of the notedeck that booted a guest.
pub const LABEL_OWNER_PID: &str = "notedeck.owner-pid";

/// How many guests one notedeck runs at once unless told otherwise. A
/// guest is ~512 MiB of RAM it may touch; four is a laptop's worth.
pub const DEFAULT_MAX_GUESTS: usize = 4;

/// The guest name for dave session `session_id`: [`NAME_PREFIX`] then the
/// id, unchanged.
///
/// Refused, rather than rewritten, if the id isn't already a valid name
/// (ASCII letters, digits, `.`, `-`, `_`, starting with a letter or digit,
/// at most [`MAX_SESSION_ID`] bytes): two sessions rewritten to one name
/// would share a guest, and the second start would tear down the first.
/// Dave's ids (UUIDs, word-ids) all pass.
pub fn guest_name(session_id: &str) -> Result<String, SandboxError> {
    let valid = !session_id.is_empty()
        && session_id.len() <= MAX_SESSION_ID
        && session_id
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && session_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_');
    if !valid {
        return Err(SandboxError::BadSessionId(session_id.to_owned()));
    }
    Ok(format!("{NAME_PREFIX}{session_id}"))
}

/// The labels a guest for `session_id` is created with, booted by the
/// process `owner_pid`.
pub fn labels(session_id: &str, owner_pid: u32) -> [GuestLabel; 3] {
    [
        GuestLabel {
            key: LABEL_APP,
            value: APP.to_owned(),
        },
        GuestLabel {
            key: LABEL_SESSION,
            value: session_id.to_owned(),
        },
        GuestLabel {
            key: LABEL_OWNER_PID,
            value: owner_pid.to_string(),
        },
    ]
}

/// One `key=value` label on a guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestLabel {
    /// The label's key, one of this module's `LABEL_*`.
    pub key: &'static str,
    /// Its value.
    pub value: String,
}

/// What the sweep knows about one sandbox when it decides.
#[derive(Debug, Clone, Copy)]
pub struct GuestRecord<'a> {
    /// The sandbox's name.
    pub name: &'a str,
    /// Its labels.
    pub labels: &'a BTreeMap<String, String>,
    /// Whether microsandbox says it is still up (anything but stopped or
    /// crashed).
    pub running: bool,
}

/// What to do with one sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Not one of ours: no [`LABEL_APP`] label of [`APP`]. Never touched.
    NotOurs,
    /// Ours, and still owned: leave it.
    Keep(KeepReason),
    /// Ours, and nobody owns it any more: stop it if it's up, then remove
    /// its record.
    Reap,
}

/// Why a guest of ours is left alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepReason {
    /// This process booted it, and it's up.
    ThisProcess,
    /// Another notedeck that is still running booted it, and it's up.
    LiveOwner,
}

/// Decide what to do with `record`, as the process `me`, where
/// `owner_alive(pid)` says whether a process with that pid exists.
///
/// A stopped guest of ours is reaped whoever owns it: a stopped guest
/// serves nobody, and its record only blocks the next create of its name.
/// A running one is kept while its owner lives. One whose owner pid label
/// is missing or garbled is reaped once it's stopped and otherwise kept,
/// because a guest whose owner can't be named can't be shown to be
/// orphaned.
///
/// A pid can be reused, so a dead owner whose pid now names another
/// process keeps its guest until that process ends too: the rule errs
/// toward leaking until the next sweep rather than killing somebody
/// else's guest. In attached mode a dead owner's guest has shut down
/// anyway, so this is a stopped record, not a VM.
pub fn verdict(record: &GuestRecord<'_>, me: u32, owner_alive: impl Fn(u32) -> bool) -> Verdict {
    if record.labels.get(LABEL_APP).map(String::as_str) != Some(APP) {
        return Verdict::NotOurs;
    }
    if !record.running {
        return Verdict::Reap;
    }
    let Some(owner) = record
        .labels
        .get(LABEL_OWNER_PID)
        .and_then(|pid| pid.parse::<u32>().ok())
    else {
        return Verdict::Keep(KeepReason::LiveOwner);
    };
    if owner == me {
        return Verdict::Keep(KeepReason::ThisProcess);
    }
    if owner_alive(owner) {
        return Verdict::Keep(KeepReason::LiveOwner);
    }
    Verdict::Reap
}

/// The bound on how many guests one notedeck runs at once. Cheap to clone;
/// clones share the bound.
#[derive(Debug, Clone)]
pub struct Capacity {
    slots: Arc<Semaphore>,
    max: usize,
}

/// One guest's place under a [`Capacity`], given back when dropped. A guest
/// holds its slot until its teardown has finished, so a guest that is
/// still shutting down still counts.
#[derive(Debug)]
pub struct Slot {
    _permit: OwnedSemaphorePermit,
}

impl Capacity {
    /// Room for `max` guests at once.
    pub fn new(max: usize) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(max)),
            max,
        }
    }

    /// The bound.
    pub fn max(&self) -> usize {
        self.max
    }

    /// How many guests hold a slot now.
    pub fn in_use(&self) -> usize {
        self.max - self.slots.available_permits()
    }

    /// Take a slot for one more guest, or [`SandboxError::AtCapacity`] at
    /// once if all are taken. Never waits.
    pub fn try_take(&self) -> Result<Slot, SandboxError> {
        let permit = Arc::clone(&self.slots)
            .try_acquire_owned()
            .map_err(|_| SandboxError::AtCapacity { max: self.max })?;
        Ok(Slot { _permit: permit })
    }
}

impl Default for Capacity {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_GUESTS)
    }
}

/// What a sweep did.
#[derive(Debug, Default)]
pub struct SweepReport {
    /// Guests whose owner was gone, or that had stopped, and that are now
    /// stopped and removed.
    pub reaped: Vec<String>,
    /// Guests of ours left alone because their owner is alive.
    pub kept: Vec<String>,
    /// Guests the sweep tried to reap and couldn't.
    pub failed: Vec<SweepFailure>,
}

/// A guest a sweep couldn't reap.
#[derive(Debug)]
pub struct SweepFailure {
    /// Its name.
    pub name: String,
    /// What went wrong stopping or removing it.
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ours(owner_pid: Option<&str>) -> BTreeMap<String, String> {
        let mut labels = BTreeMap::new();
        labels.insert(LABEL_APP.to_owned(), APP.to_owned());
        labels.insert(LABEL_SESSION.to_owned(), "silk-blossom-cry".to_owned());
        if let Some(pid) = owner_pid {
            labels.insert(LABEL_OWNER_PID.to_owned(), pid.to_owned());
        }
        labels
    }

    fn record<'a>(labels: &'a BTreeMap<String, String>, running: bool) -> GuestRecord<'a> {
        GuestRecord {
            name: "agentium-silk-blossom-cry",
            labels,
            running,
        }
    }

    const ME: u32 = 100;
    const DEAD: u32 = 200;
    const ALIVE: u32 = 300;

    fn alive(pid: u32) -> bool {
        pid == ME || pid == ALIVE
    }

    #[test]
    fn a_session_id_is_the_name_unchanged() {
        assert_eq!(
            guest_name("silk-blossom-cry").unwrap(),
            "agentium-silk-blossom-cry"
        );
        assert_eq!(
            guest_name("0b7e6a52-3c1f-4d2e-9a8b-5f6e7d8c9b0a").unwrap(),
            "agentium-0b7e6a52-3c1f-4d2e-9a8b-5f6e7d8c9b0a"
        );
    }

    /// Ids that would have to be rewritten are refused, so two sessions
    /// can never share a guest name.
    #[test]
    fn an_id_that_is_not_a_name_is_refused() {
        for bad in [
            "",
            "-leading-dash",
            "has space",
            "slash/y",
            "dave:board",
            "é",
            &"x".repeat(MAX_SESSION_ID + 1),
        ] {
            let err = guest_name(bad).unwrap_err();
            assert!(matches!(err, SandboxError::BadSessionId(_)), "{bad:?}");
        }
        assert!(guest_name(&"x".repeat(MAX_SESSION_ID)).is_ok());
    }

    #[test]
    fn labels_name_the_app_the_session_and_the_owner() {
        let [app, session, owner] = labels("s1", 42);
        assert_eq!((app.key, app.value.as_str()), (LABEL_APP, APP));
        assert_eq!((session.key, session.value.as_str()), (LABEL_SESSION, "s1"));
        assert_eq!((owner.key, owner.value.as_str()), (LABEL_OWNER_PID, "42"));
        for label in [LABEL_APP, LABEL_SESSION, LABEL_OWNER_PID] {
            // microsandbox reserves these prefixes and rejects the create.
            for reserved in ["sandbox.", "microsandbox.", "service."] {
                assert!(!label.starts_with(reserved), "{label}");
            }
        }
    }

    /// A user's own sandbox is never touched, even one named like ours.
    #[test]
    fn without_our_app_label_it_is_not_ours() {
        let mut theirs = BTreeMap::new();
        assert_eq!(
            verdict(&record(&theirs, false), ME, alive),
            Verdict::NotOurs
        );
        theirs.insert(LABEL_APP.to_owned(), "something-else".to_owned());
        theirs.insert(LABEL_OWNER_PID.to_owned(), DEAD.to_string());
        assert_eq!(verdict(&record(&theirs, true), ME, alive), Verdict::NotOurs);
    }

    /// A stopped guest is only a record that blocks its name: reaped,
    /// whoever booted it.
    #[test]
    fn a_stopped_guest_of_ours_is_reaped_whoever_owns_it() {
        for owner in [Some("100"), Some("300"), Some("200"), Some("junk"), None] {
            let labels = ours(owner);
            assert_eq!(
                verdict(&record(&labels, false), ME, alive),
                Verdict::Reap,
                "{owner:?}"
            );
        }
    }

    #[test]
    fn a_running_guest_is_kept_while_its_owner_lives() {
        let mine = ours(Some("100"));
        assert_eq!(
            verdict(&record(&mine, true), ME, alive),
            Verdict::Keep(KeepReason::ThisProcess)
        );
        let another_notedecks = ours(Some("300"));
        assert_eq!(
            verdict(&record(&another_notedecks, true), ME, alive),
            Verdict::Keep(KeepReason::LiveOwner)
        );
    }

    /// The leak this module exists for: a notedeck died and its guest is
    /// still up (or still recorded as up).
    #[test]
    fn a_running_guest_whose_owner_died_is_reaped() {
        let orphan = ours(Some("200"));
        assert_eq!(verdict(&record(&orphan, true), ME, alive), Verdict::Reap);
    }

    /// No owner to check, so no proof it's orphaned: kept while it runs.
    #[test]
    fn a_running_guest_with_no_readable_owner_is_kept() {
        for owner in [None, Some("junk"), Some("-1")] {
            let labels = ours(owner);
            assert_eq!(
                verdict(&record(&labels, true), ME, alive),
                Verdict::Keep(KeepReason::LiveOwner),
                "{owner:?}"
            );
        }
    }

    #[test]
    fn the_bound_refuses_at_once_and_frees_on_drop() {
        let capacity = Capacity::new(2);
        let a = capacity.try_take().expect("first");
        let _b = capacity
            .clone()
            .try_take()
            .expect("second, through a clone");
        assert_eq!(capacity.in_use(), 2);

        let err = capacity.try_take().unwrap_err();
        assert!(matches!(err, SandboxError::AtCapacity { max: 2 }), "{err}");

        drop(a);
        assert_eq!(capacity.in_use(), 1);
        capacity.try_take().expect("a dropped slot is free again");
    }
}
