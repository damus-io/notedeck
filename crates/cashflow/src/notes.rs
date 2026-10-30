//! Notes on a transaction: free text the owner leaves on one record, kept
//! as NIP-22 kind-1111 comments sealed into the vault.
//!
//! A note is a regular note, not a [`crate::vault::Record`]: every note is
//! kept, so a transaction has an append-only thread rather than one newest
//! revision, and no cashflow kind is spent on it. Each one is signed by
//! whoever wrote it, so a vault shared with a partner later reads as a
//! conversation with no change to the notes already written.
//!
//! ## The note
//!
//! Kind 1111, its `content` the text, and the record it is on as NIP-22's
//! external-identifier scope, root and parent alike:
//!
//! ```text
//! ["I", "cashflow:txn:<record-id>"]  ["K", "cashflow:txn"]
//! ["i", "cashflow:txn:<record-id>"]  ["k", "cashflow:txn"]
//! ["v", "1"]
//! ```
//!
//! The `I` value carries a prefix, so it is never 64 hex characters, which
//! nostrdb would store as an id that a string tag filter misses. Reads ask
//! for kind 1111 by `#I` (one record's thread) or `#K` (every noted record)
//! and check the author in Rust: an authors + kinds + tags filter scans the
//! author's notes of the kind anyway.
//!
//! ## Never derive input
//!
//! Notes never change what a transaction is categorized as. Kind 1111 is
//! outside [`crate::kinds::ALL`], so [`crate::EditsCache`] never sees one and
//! a post moves neither of its generations. [`NotesCache`] is the notes'
//! own live view, with its own generation.

use std::collections::HashSet;

use nostrdb::{Filter, Ndb, Note as NdbNote, NoteBuilder, NoteKey, Subscription, Transaction};
use nostrdb_net::Pubkey;
use nostrdb_net::private_note::next_stamp;

use crate::vault::{self, SCHEMA_VERSION, Vault, VaultError, Written, tag_value};

/// NIP-22's comment kind.
pub const KIND: u32 = 1111;

/// The external-identifier kind every note's `K` and `k` tags name: a
/// cashflow transaction.
pub const SCOPE: &str = "cashflow:txn";

/// How many delivered keys one `poll_for_notes` call takes at most, as
/// [`crate::EditsCache`] polls: a quiet frame's poll allocates nothing.
const POLL_BATCH: u32 = 64;

/// One note on a transaction, as read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    /// Who wrote it.
    pub author: Pubkey,
    /// When, in unix seconds.
    pub created_at: u64,
    /// What it says.
    pub text: String,
}

/// The `I` (and `i`) value naming `record`: `cashflow:txn:<record>`.
pub fn scope_of(record: &str) -> String {
    format!("{SCOPE}:{record}")
}

/// Post `text` as a note on `record`, from the vault's account: signed,
/// sealed and ingested, readable once nostrdb's ingest lands.
///
/// It is stamped past the newest note already on the record and `after`
/// (pass 0, or the last post's [`Written::created_at`] when it may not have
/// landed yet), so a thread reads in the order it was written even when two
/// notes go in within a second. The text is trimmed; an empty one is
/// refused.
pub fn post(
    vault: &Vault,
    ndb: &Ndb,
    record: &str,
    text: &str,
    after: u64,
) -> Result<Written, VaultError> {
    vault::check_id(record)?;
    let text = text.trim();
    if text.is_empty() {
        return Err(VaultError::EmptyNote);
    }
    let stored = Transaction::new(ndb)
        .ok()
        .and_then(|txn| newest_on(ndb, &txn, vault.author(), record))
        .unwrap_or(0);
    let created_at = next_stamp(unix_now(), stored, after);
    let scope = scope_of(record);
    let note = NoteBuilder::new()
        .kind(KIND)
        .content(text)
        .start_tag()
        .tag_str("I")
        .tag_str(&scope)
        .start_tag()
        .tag_str("K")
        .tag_str(SCOPE)
        .start_tag()
        .tag_str("i")
        .tag_str(&scope)
        .start_tag()
        .tag_str("k")
        .tag_str(SCOPE)
        .start_tag()
        .tag_str("v")
        .tag_str(SCHEMA_VERSION);
    vault.post(ndb, note, created_at)
}

/// Every note `author` left on `record`, oldest first; a same-second pair
/// in id order, so every device reads one order.
pub fn notes_of(ndb: &Ndb, txn: &Transaction, author: &Pubkey, record: &str) -> Vec<Note> {
    let scope = scope_of(record);
    let filter = Filter::new()
        .kinds([KIND as u64])
        .tags([scope.as_str()], 'I')
        .build();
    let mut found: Vec<Keyed> = ndb
        .fold(txn, &[filter], Vec::new(), |mut found, note| {
            if note.pubkey() == author.bytes() {
                found.push(Keyed {
                    id: *note.id(),
                    note: Note {
                        author: Pubkey::new(*note.pubkey()),
                        created_at: note.created_at(),
                        text: note.content().to_owned(),
                    },
                });
            }
            found
        })
        .unwrap_or_default();
    found.sort_unstable_by_key(|keyed| (keyed.note.created_at, keyed.id));
    found.into_iter().map(|keyed| keyed.note).collect()
}

/// A note beside its id, which breaks a same-second tie.
struct Keyed {
    id: [u8; 32],
    note: Note,
}

/// The record ids `author` has left a note on.
pub fn noted(ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> HashSet<String> {
    ndb.fold(txn, &[scope_filter()], HashSet::new(), |mut noted, note| {
        if note.pubkey() == author.bytes()
            && let Some(record) = record_of(&note)
        {
            noted.insert(record.to_owned());
        }
        noted
    })
    .unwrap_or_default()
}

/// The `created_at` of the newest note `author` left on `record`.
fn newest_on(ndb: &Ndb, txn: &Transaction, author: &Pubkey, record: &str) -> Option<u64> {
    let scope = scope_of(record);
    let filter = Filter::new()
        .kinds([KIND as u64])
        .tags([scope.as_str()], 'I')
        .build();
    ndb.fold(txn, &[filter], None, |newest: Option<u64>, note| {
        if note.pubkey() != author.bytes() {
            return newest;
        }
        Some(newest.map_or(note.created_at(), |n| n.max(note.created_at())))
    })
    .ok()
    .flatten()
}

/// Every note on a cashflow transaction, by anyone: kind 1111 with
/// `K = cashflow:txn`.
fn scope_filter() -> Filter {
    Filter::new()
        .kinds([KIND as u64])
        .tags([SCOPE], 'K')
        .build()
}

/// The record `note` is on, from its `I` tag, borrowed from the note.
fn record_of<'a>(note: &NdbNote<'a>) -> Option<&'a str> {
    tag_value(&note.tags(), "I")?
        .strip_prefix(SCOPE)?
        .strip_prefix(':')
}

/// The system clock, in unix seconds.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Which records an account has noted, kept current from an ndb
/// subscription for the app: polled each frame, it refolds only when a
/// note arrived, so a quiet frame costs one non-blocking poll. It keeps
/// [`crate::EditsCache`]'s ordering discipline: subscribe before the first
/// fold's transaction, and hold back a delivered note until the poll's
/// transaction can see it.
pub struct NotesCache {
    author: Pubkey,
    /// The live subscription; `None` when nostrdb refused it, and then
    /// every poll refolds.
    sub: Option<Subscription>,
    /// Delivered keys the last poll's transaction couldn't see yet.
    pending: Vec<NoteKey>,
    /// The records with notes; `None` until the first poll.
    noted: Option<HashSet<String>>,
    generation: u64,
}

impl NotesCache {
    /// A cache of `author`'s notes, subscribed now and folded on the first
    /// [`Self::poll`]. Create it before opening the transaction that poll
    /// is given, as [`crate::EditsCache::new`] says.
    pub fn new(ndb: &Ndb, author: Pubkey) -> Self {
        NotesCache {
            sub: ndb.subscribe(&[scope_filter()]).ok(),
            author,
            pending: Vec::new(),
            noted: None,
            generation: 0,
        }
    }

    /// Take what the subscription delivered and refold if any of it is
    /// visible in `txn`. True when [`Self::generation`] moved: some note
    /// arrived, so a thread on screen reads itself again.
    #[profiling::function]
    pub fn poll(&mut self, ndb: &Ndb, txn: &Transaction) -> bool {
        if let Some(sub) = self.sub {
            loop {
                let keys = ndb.poll_for_notes(sub, POLL_BATCH);
                let done = keys.len() < POLL_BATCH as usize;
                self.pending.extend(keys);
                if done {
                    break;
                }
            }
            let delivered = self.pending.len();
            self.pending
                .retain(|&key| ndb.get_note_by_key(txn, key).is_err());
            if self.noted.is_some() && self.pending.len() == delivered {
                return false;
            }
        }
        self.noted = Some(noted(ndb, txn, &self.author));
        self.generation += 1;
        true
    }

    /// The records with notes; `None` before the first [`Self::poll`].
    pub fn noted(&self) -> Option<&HashSet<String>> {
        self.noted.as_ref()
    }

    /// Bumps each time a note arrives; 0 before the first fold.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The account whose notes these are.
    pub fn author(&self) -> &Pubkey {
        &self.author
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::EditsCache;
    use crate::test_util::{INGEST_TIMEOUT as WAIT, TempNdb};
    use nostrdb::IngestMetadata;

    const SECRET: [u8; 32] = [5u8; 32];
    const OTHER_SECRET: [u8; 32] = [6u8; 32];

    /// Subscribe to every transaction note, before anything that should
    /// land is ingested.
    fn subscribe(ndb: &Ndb) -> Subscription {
        ndb.subscribe(&[scope_filter()]).unwrap()
    }

    /// A post reads back as the thread's note, and two posts made within a
    /// second keep the order they were written in.
    #[tokio::test]
    async fn posts_read_back_in_order() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let sub = subscribe(ndb);

        let first = post(&vault, ndb, "card:1", "  receipt in the drawer ", 0).unwrap();
        let second = post(&vault, ndb, "card:1", "returned one item", first.created_at).unwrap();
        assert!(second.created_at > first.created_at);
        post(&vault, ndb, "card:2", "gift", 0).unwrap();
        ndb.wait_for_all_notes_within(sub, 3, WAIT).await.unwrap();

        let txn = Transaction::new(ndb).unwrap();
        let thread = notes_of(ndb, &txn, vault.author(), "card:1");
        let texts: Vec<&str> = thread.iter().map(|n| n.text.as_str()).collect();
        assert_eq!(texts, ["receipt in the drawer", "returned one item"]);
        assert_eq!(&thread[0].author, vault.author());
        assert_eq!(thread[1].created_at, second.created_at);

        let noted = noted(ndb, &txn, vault.author());
        assert_eq!(noted.len(), 2);
        assert!(noted.contains("card:1") && noted.contains("card:2"));

        // Another account's reads see none of them.
        let stranger = Vault::new(&OTHER_SECRET).unwrap();
        assert!(notes_of(ndb, &txn, stranger.author(), "card:1").is_empty());
        assert!(super::noted(ndb, &txn, stranger.author()).is_empty());
    }

    /// A later post stamps past the thread's newest note even without
    /// `after`: what keeps two CLI runs in one second in order.
    #[tokio::test]
    async fn a_post_stamps_past_the_stored_thread() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let sub = subscribe(ndb);
        let first = post(&vault, ndb, "card:1", "one", 0).unwrap();
        ndb.wait_for_all_notes_within(sub, 1, WAIT).await.unwrap();
        let second = post(&vault, ndb, "card:1", "two", 0).unwrap();
        assert!(second.created_at > first.created_at);
    }

    /// The note is a kind-1111 comment scoped to the record, root and
    /// parent, with the schema version, and its content the text.
    #[tokio::test]
    async fn the_note_is_a_nip22_comment_on_the_record() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let sub = subscribe(ndb);
        post(&vault, ndb, "amex:2024-01-02-0001", "split with sam", 0).unwrap();
        ndb.wait_for_all_notes_within(sub, 1, WAIT).await.unwrap();

        let txn = Transaction::new(ndb).unwrap();
        let notes = ndb.query(&txn, &[scope_filter()], 10).unwrap();
        let [hit] = &notes[..] else {
            panic!("one note, got {}", notes.len());
        };
        let note = &hit.note;
        assert_eq!(note.kind(), KIND);
        assert_eq!(note.content(), "split with sam");
        let tags: Vec<Vec<String>> = note
            .tags()
            .iter()
            .map(|t| {
                (0..t.count())
                    .filter_map(|i| t.get_str(i).map(str::to_owned))
                    .collect()
            })
            .collect();
        let scope = "cashflow:txn:amex:2024-01-02-0001";
        assert_eq!(
            tags,
            [
                ["I", scope],
                ["K", SCOPE],
                ["i", scope],
                ["k", SCOPE],
                ["v", SCHEMA_VERSION],
            ]
        );
        assert_eq!(record_of(note), Some("amex:2024-01-02-0001"));
    }

    /// What reaches a relay is the sealed envelope alone: the frame never
    /// carries the text, and the note nostrdb unwrapped is a rumor, which
    /// the host's fan-out (`is_rumor`) never sends in the clear.
    #[tokio::test]
    async fn no_plaintext_note_is_queued_for_a_relay() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let sub = subscribe(ndb);
        let written = post(&vault, ndb, "card:1", "the secret text", 0).unwrap();
        assert!(
            !written.frame.contains("the secret text"),
            "plaintext on the wire"
        );
        assert!(
            written.frame.contains("\"kind\":1081"),
            "the frame is an SNS envelope: {}",
            written.frame
        );
        ndb.wait_for_all_notes_within(sub, 1, WAIT).await.unwrap();
        let txn = Transaction::new(ndb).unwrap();
        let notes = ndb.query(&txn, &[scope_filter()], 10).unwrap();
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].note.is_rumor(),
            "the stored note is the sealed rumor"
        );

        // A nostrdb without the vault root sees only the envelope.
        let reader_db = TempNdb::new();
        let reader = &reader_db.ndb;
        let envelopes = reader
            .subscribe(&[Filter::new()
                .kinds([nostrdb_net::sns::SNS_ENVELOPE_KIND as u64])
                .build()])
            .unwrap();
        reader
            .process_event_with(&written.frame, IngestMetadata::new().client(true))
            .unwrap();
        reader
            .wait_for_all_notes_within(envelopes, 1, WAIT)
            .await
            .unwrap();
        let txn = Transaction::new(reader).unwrap();
        assert!(notes_of(reader, &txn, vault.author(), "card:1").is_empty());
    }

    /// A post moves neither of the edits cache's generations: notes are
    /// never derive input. The notes cache's own generation moves.
    #[tokio::test]
    async fn a_post_leaves_the_edits_generation_alone() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let mut edits = EditsCache::new(ndb, *vault.author());
        let mut notes = NotesCache::new(ndb, *vault.author());
        {
            let txn = Transaction::new(ndb).unwrap();
            edits.poll(ndb, &txn);
            assert!(notes.poll(ndb, &txn), "the first poll folds");
        }
        let (generation, vault_generation) = (edits.generation(), edits.vault_generation());
        assert_eq!(notes.generation(), 1);
        assert!(notes.noted().is_some_and(HashSet::is_empty));

        let sub = subscribe(ndb);
        post(&vault, ndb, "card:1", "a note", 0).unwrap();
        ndb.wait_for_all_notes_within(sub, 1, WAIT).await.unwrap();

        let txn = Transaction::new(ndb).unwrap();
        assert_eq!(edits.poll(ndb, &txn), crate::edits_cache::Change::default());
        assert_eq!(edits.generation(), generation);
        assert_eq!(edits.vault_generation(), vault_generation);
        assert!(notes.poll(ndb, &txn));
        assert_eq!(notes.generation(), 2);
        assert!(notes.noted().is_some_and(|n| n.contains("card:1")));
        // A quiet poll changes nothing.
        assert!(!notes.poll(ndb, &txn));
        assert_eq!(notes.generation(), 2);
    }

    /// An empty note or a bad record id stores nothing.
    #[test]
    fn empty_notes_and_bad_ids_are_refused() {
        let db = TempNdb::new();
        let vault = Vault::new(&SECRET).unwrap();
        assert!(matches!(
            post(&vault, &db.ndb, "card:1", "  \n", 0),
            Err(VaultError::EmptyNote)
        ));
        assert!(matches!(
            post(&vault, &db.ndb, "card 1", "hi", 0),
            Err(VaultError::BadId(_))
        ));
    }
}
