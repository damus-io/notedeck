//! An account's private latest-wins note, read and written.
//!
//! Several apps keep a small piece of per-account state as a
//! parameterized-replaceable note that only the account can read: headway's
//! selected board and saved list views, chrome's keymap overrides. Each is a
//! [`Slot`] — an author, a kind and a `d` tag — whose newest revision counts.
//! The note is signed by the account and PNS-wrapped ([`nostrdb_net::pns`]),
//! so it never leaves the device in the clear; nostrdb unwraps it on ingest
//! once the account key is registered with `Ndb::add_key`, and it reads back
//! like any other note of the account's. A watch-only account can't unwrap it
//! and reads nothing.
//!
//! Two rules make "newest" agree everywhere:
//!
//! - **Reads order by `(created_at, id)`.** nostrdb keeps every revision and
//!   hands same-second ones back in no particular order; the id tie-break
//!   resolves them the same way on every device and every run.
//! - **Writes stamp strictly past both the stored revision and the caller's
//!   `after`** ([`next_stamp`]). nostrdb ingests asynchronously, so a write
//!   made before the last one has landed can't see it stored; the stamp the
//!   last [`write`] returned, passed back as `after`, keeps the two apart.

use std::collections::BTreeMap;

use nostrdb::{Filter, Ndb, Note, NoteBuildOptions, NoteBuilder, Transaction};
use nostrdb_net::Pubkey;

/// One private latest-wins note of an account: every revision with this
/// author, kind and `d` tag supersedes the last.
#[derive(Clone, Copy, Debug)]
pub struct Slot<'a> {
    /// The account that signs the note, and the only one that can read it.
    pub author: &'a Pubkey,
    /// The note's kind: a parameterized-replaceable one (30000–39999).
    pub kind: u32,
    /// The note's `d` tag. A string: a 64-hex `d` is stored by nostrdb as an
    /// id and never matches.
    pub d: &'a str,
}

/// The newest revision of a [`Slot`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revision {
    /// When it was written: the baseline the next [`write`] stamps past.
    pub created_at: u64,
    /// Its note id, which breaks a same-second tie.
    pub id: [u8; 32],
    /// What it holds. Empty content is still a revision (a tombstone, say),
    /// so a later write stamps past it.
    pub content: String,
}

/// The newest revision of one `d` among an account's notes of a kind (see
/// [`latest_per_d`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotRevision {
    /// The `d` tag it is the newest revision of.
    pub d: String,
    /// The revision.
    pub revision: Revision,
}

/// Why a [`write`] or an [`ingest`] stored nothing.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// Signing the inner note failed.
    #[error("couldn't build the note")]
    Build,
    /// Serializing a note to JSON failed.
    #[error("note serialization failed: {0}")]
    Serialize(String),
    /// PNS encryption or 1080-envelope signing failed (see [`nostrdb_net::pns::wrap`]).
    #[error("PNS wrap failed")]
    Wrap,
    /// nostrdb rejected the envelope ingest frame.
    #[error("ndb ingest failed: {0}")]
    Ingest(String),
}

/// The filter for every note of `kind` that `author` signed. The `d` tag is
/// checked in Rust: nostrdb answers an authors + kinds + tags filter by
/// scanning every note of the author's kinds anyway, slower than checking the
/// handful of matches ourselves.
fn filter(author: &Pubkey, kind: u32) -> Filter {
    Filter::new()
        .authors([author.bytes()])
        .kinds([kind as u64])
        .build()
}

/// `note`'s `d` tag value, when it's a string.
fn d_tag<'a>(note: &Note<'a>) -> Option<&'a str> {
    note.tags().iter().find_map(|tag| {
        (tag.count() >= 2 && tag.get_str(0) == Some("d"))
            .then(|| tag.get_str(1))
            .flatten()
    })
}

/// Whether `note` supersedes `than`: later `created_at`, ties broken by the
/// larger id.
fn supersedes(note: &Note<'_>, than: &Note<'_>) -> bool {
    (note.created_at(), note.id()) > (than.created_at(), than.id())
}

/// The owned [`Revision`] `note` is.
fn revision(note: &Note<'_>) -> Revision {
    Revision {
        created_at: note.created_at(),
        id: *note.id(),
        content: note.content().to_owned(),
    }
}

/// The newest revision of `slot`, or `None` when it has none (or the account
/// is watch-only, so nostrdb couldn't unwrap it).
pub fn latest(ndb: &Ndb, txn: &Transaction, slot: Slot<'_>) -> Option<Revision> {
    let newest = ndb
        .fold(
            txn,
            &[filter(slot.author, slot.kind)],
            None::<Note<'_>>,
            |best, note| {
                if d_tag(&note) != Some(slot.d) {
                    return best;
                }
                match best {
                    Some(best) if !supersedes(&note, &best) => Some(best),
                    _ => Some(note),
                }
            },
        )
        .ok()??;
    Some(revision(&newest))
}

/// The newest revision of every `d` among `author`'s notes of `kind`, in `d`
/// order. For a kind that keeps many slots, such as one note per saved view.
pub fn latest_per_d(ndb: &Ndb, txn: &Transaction, author: &Pubkey, kind: u32) -> Vec<SlotRevision> {
    let newest = ndb.fold(
        txn,
        &[filter(author, kind)],
        BTreeMap::<&str, Note<'_>>::new(),
        |mut newest, note| {
            let Some(d) = d_tag(&note) else {
                return newest;
            };
            if newest.get(d).is_none_or(|best| supersedes(&note, best)) {
                newest.insert(d, note);
            }
            newest
        },
    );
    newest
        .unwrap_or_default()
        .into_iter()
        .map(|(d, note)| SlotRevision {
            d: d.to_owned(),
            revision: revision(&note),
        })
        .collect()
}

/// The `created_at` a write made at `now` stamps: strictly past both the
/// `stored` revision's and `after`, the stamp of the caller's own last write,
/// so a re-save in the same second, or one made before the last has been
/// ingested, still supersedes.
pub fn next_stamp(now: u64, stored: u64, after: u64) -> u64 {
    now.max(stored.max(after) + 1)
}

/// Write `content` as `slot`'s newest revision, signed with the author's
/// `secret`, PNS-wrapped and ingested (see [`ingest`]). Returns the
/// `created_at` it was stamped with, which a caller making rapid writes
/// passes back as the next write's `after` (see [`next_stamp`]); pass 0 when
/// there was no earlier write.
pub fn write(
    ndb: &Ndb,
    secret: &[u8; 32],
    slot: Slot<'_>,
    content: &str,
    after: u64,
) -> Result<u64, WriteError> {
    let stored = Transaction::new(ndb)
        .ok()
        .and_then(|txn| latest(ndb, &txn, slot))
        .map_or(0, |revision| revision.created_at);
    let created_at = next_stamp(unix_time_secs(), stored, after);
    let note = NoteBuilder::new()
        .kind(slot.kind)
        .content(content)
        .options(NoteBuildOptions::default())
        .created_at(created_at)
        .start_tag()
        .tag_str("d")
        .tag_str(slot.d)
        .sign(secret)
        .build()
        .ok_or(WriteError::Build)?;
    let json = note
        .json()
        .map_err(|err| WriteError::Serialize(err.to_string()))?;
    ingest(ndb, secret, &json)?;
    Ok(created_at)
}

/// PNS-wrap a signed inner note (`inner_json`) into a kind-1080 envelope and
/// ingest it into the local nostrdb. `secret` is the account's 32-byte device
/// secret; its PNS keypair is derived here
/// ([`nostrdb_net::pns::derive_pns_keys`]).
///
/// nostrdb's `process_pns` unwraps the envelope on ingest, so the inner note
/// is queryable on this device at once, exactly as an inbound relay envelope
/// would be. The `"_pns"` subid of the `["EVENT","_pns",{…}]` frame is a
/// local marker: the envelope carries no seen-on relay.
pub fn ingest(ndb: &Ndb, secret: &[u8; 32], inner_json: &str) -> Result<(), WriteError> {
    let pns_keys = nostrdb_net::pns::derive_pns_keys(secret);
    let envelope =
        nostrdb_net::pns::wrap(&pns_keys, inner_json, unix_time_secs()).ok_or(WriteError::Wrap)?;
    let envelope_json = envelope
        .json()
        .map_err(|err| WriteError::Serialize(err.to_string()))?;
    ndb.process_event(&format!("[\"EVENT\",\"_pns\",{envelope_json}]"))
        .map_err(|err| WriteError::Ingest(err.to_string()))
}

/// Current wall-clock time in whole seconds since the Unix epoch (nostr's
/// `created_at` unit), or 0 if the clock is before the epoch.
fn unix_time_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use nostrdb::{Config, SubscriptionStream};
    use nostrdb_net::FullKeypair;

    /// How long a test waits for nostrdb to ingest what it wrote.
    const INGEST_TIMEOUT: Duration = Duration::from_secs(10);

    /// A parameterized-replaceable kind for the tests.
    const KIND: u32 = 30078;

    /// A small ndb: the default 32 GiB map fills Windows CI's disk.
    fn test_config() -> Config {
        if cfg!(target_os = "windows") {
            Config::new().set_mapsize(32 * 1024 * 1024)
        } else {
            Config::new()
        }
    }

    /// A fresh ndb in `dir` that unwraps `kp`'s PNS notes, and a stream of
    /// its notes of [`KIND`], subscribed before anything is written.
    struct Fixture {
        ndb: Ndb,
        stream: SubscriptionStream,
        kp: FullKeypair,
        _dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().expect("tmp dir");
            let kp = FullKeypair::generate();
            let ndb = Ndb::new(dir.path().to_str().expect("path"), &test_config()).expect("ndb");
            assert!(ndb.add_key(&kp.secret_key.secret_bytes()));
            let sub = ndb.subscribe(&[filter(&kp.pubkey, KIND)]).expect("sub");
            let stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);
            Self {
                ndb,
                stream,
                kp,
                _dir: dir,
            }
        }

        fn secret(&self) -> [u8; 32] {
            self.kp.secret_key.secret_bytes()
        }

        fn slot<'a>(&'a self, d: &'a str) -> Slot<'a> {
            Slot {
                author: &self.kp.pubkey,
                kind: KIND,
                d,
            }
        }

        /// Ingest a revision of `d` stamped exactly `created_at`, bypassing
        /// [`write`]'s stamping. Returns its id.
        fn ingest_at(&self, d: &str, content: &str, created_at: u64) -> [u8; 32] {
            let note = NoteBuilder::new()
                .kind(KIND)
                .content(content)
                .created_at(created_at)
                .start_tag()
                .tag_str("d")
                .tag_str(d)
                .sign(&self.secret())
                .build()
                .expect("note");
            ingest(&self.ndb, &self.secret(), &note.json().expect("json")).expect("ingest");
            *note.id()
        }

        async fn wait(&mut self, n: usize) {
            self.stream
                .wait_for_notes(n, INGEST_TIMEOUT)
                .await
                .expect("ingested before the deadline");
        }

        fn latest(&self, d: &str) -> Option<Revision> {
            let txn = Transaction::new(&self.ndb).expect("txn");
            latest(&self.ndb, &txn, self.slot(d))
        }
    }

    #[test]
    fn a_stamp_passes_the_stored_revision_and_the_last_write() {
        assert_eq!(next_stamp(100, 0, 0), 100);
        assert_eq!(next_stamp(100, 100, 0), 101);
        assert_eq!(next_stamp(100, 0, 105), 106);
        assert_eq!(next_stamp(100, 107, 105), 108);
    }

    /// Two revisions in the same second: the larger id wins, whichever was
    /// ingested last.
    #[tokio::test]
    async fn a_same_second_tie_goes_to_the_larger_id() {
        let mut fx = Fixture::new();
        let a = fx.ingest_at("x", "a", 1000);
        let b = fx.ingest_at("x", "b", 1000);
        fx.wait(2).await;

        let want_id = a.max(b);
        let want = if a > b { "a" } else { "b" };
        let newest = fx.latest("x").expect("a revision");
        assert_eq!(newest.id, want_id);
        assert_eq!(newest.content, want);

        let txn = Transaction::new(&fx.ndb).expect("txn");
        let all = latest_per_d(&fx.ndb, &txn, &fx.kp.pubkey, KIND);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].revision.id, want_id);
    }

    /// Back to back, each written before the last is stored: only the stamp
    /// the last returned keeps them apart, and the last write wins.
    #[tokio::test]
    async fn after_stamps_past_an_uningested_write() {
        let mut fx = Fixture::new();
        let secret = fx.secret();
        let mut last = 0;
        for content in ["a", "b", "c", "d"] {
            let stamped = write(&fx.ndb, &secret, fx.slot("x"), content, last).expect("write");
            assert!(stamped > last);
            last = stamped;
        }
        fx.wait(4).await;
        let newest = fx.latest("x").expect("a revision");
        assert_eq!(newest.content, "d");
        assert_eq!(newest.created_at, last);
    }

    /// A write stamps past a stored revision from the future, and each `d` is
    /// its own slot.
    #[tokio::test]
    async fn slots_are_kept_apart_and_writes_supersede_the_stored_one() {
        let mut fx = Fixture::new();
        let future = unix_time_secs() + 60;
        fx.ingest_at("x", "old", future);
        fx.ingest_at("y", "other", future + 5);
        fx.wait(2).await;

        let secret = fx.secret();
        let stamped = write(&fx.ndb, &secret, fx.slot("x"), "new", 0).expect("write");
        assert_eq!(stamped, future + 1);
        fx.wait(1).await;

        assert_eq!(fx.latest("x").expect("x").content, "new");
        assert_eq!(fx.latest("y").expect("y").content, "other");
        assert_eq!(fx.latest("z"), None);

        let txn = Transaction::new(&fx.ndb).expect("txn");
        let all = latest_per_d(&fx.ndb, &txn, &fx.kp.pubkey, KIND);
        let ds: Vec<&str> = all.iter().map(|s| s.d.as_str()).collect();
        let contents: Vec<&str> = all.iter().map(|s| s.revision.content.as_str()).collect();
        assert_eq!(ds, ["x", "y"]);
        assert_eq!(contents, ["new", "other"]);
    }
}
