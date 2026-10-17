//! The account's private cashflow vault: small typed records over nostr.
//!
//! Everything cashflow *decides* — settings now; rules, overrides, splits,
//! transfer links, segment manifests and account metadata in later cards — is
//! one [`Record`] each: a parameterized-replaceable note of a kind from
//! [`crate::kinds`], `d = <prefix>:<id>`, its fields as tags and its content
//! empty. Bulk transaction data never goes here; it lives in journal segments.
//!
//! ## Tags, read in place
//!
//! A record's fields are tags (`["value", "CAD"]`), not JSON content, so a
//! read borrows each field straight out of the note in the transaction:
//! [`latest`] and [`latest_all`] hand back records whose `&str`s point into
//! nostrdb, with nothing parsed or copied. Every record also carries
//! `["v", "1"]` ([`SCHEMA_VERSION`]); a reader looks up the tags it knows, so
//! one a newer writer added is simply never asked for.
//!
//! nostrdb stores a 64-hex tag value as a 32-byte id, which a string read then
//! misses. So ids live behind a `d` prefix ([`Record::PREFIX`]), and a field
//! value that is 64 hex characters is refused at write ([`VaultError::HexValue`])
//! rather than stored where its own reader can't see it.
//!
//! ## Sealing
//!
//! The vault is a team-of-one SNS channel, as notebook's is
//! (`crates/notebook/src/store.rs`). Its root is *derived* from the account
//! secret ([`vault_root`]), so every device holding the secret lands on the
//! same channel without coordinating, and sharing the vault with another
//! person later is a hand-off of the root rather than a re-seal.
//!
//! Each record is signed by the account, sealed into a kind-1081 envelope
//! ([`nostrdb_net::sns::wrap_rumor`]) and ingested as that envelope. nostrdb
//! unwraps it back into the record once the root is registered
//! ([`Vault::register`]), so reads are plain queries by author and kind. A
//! relay only ever sees the envelope.
//!
//! ## Newest wins
//!
//! Reads go straight through nostrdb_net's [`query_replaceable`] fold, which
//! keeps the newest revision of each `d` by `(created_at, id)`, so every
//! device and run settles a same-second tie the same way. A write is stamped
//! strictly past both the stored revision and the caller's own last write
//! ([`nostrdb_net::private_note::next_stamp`], the one stamp rule notedeck's
//! private notes share), so a quick re-save still supersedes.

use nostrdb::{
    Filter, IngestMetadata, Ndb, Note, NoteBuildOptions, NoteBuilder, Tags, Transaction,
};
use nostrdb_net::private_note::next_stamp;
use nostrdb_net::sns::{self, SnsKeys};
use nostrdb_net::{
    ClientMessage, FullKeypair, Pubkey, query_replaceable, query_replaceable_filtered,
};

/// The label the vault root is derived under: one vault per account.
const VAULT_LABEL: &str = "cashflow";

/// The schema version every record is written with, as its `v` tag. Adding
/// a field doesn't bump it, since readers never ask for tags they don't know;
/// changing what an existing field means does.
pub const SCHEMA_VERSION: &str = "1";

/// The account's vault `team_root`, derived from its `secret`. The same on
/// every device that holds the secret.
pub fn vault_root(secret: &[u8; 32]) -> [u8; 32] {
    sns::derive_board_root(secret, VAULT_LABEL)
}

/// A typed vault record: its fields as tags, and where it is stored.
///
/// `'a` is the transaction a read borrows from: a record type holds `&'a str`
/// fields, so reading one copies nothing.
pub trait Record<'a>: Sized {
    /// The addressable kind it is stored under, from [`crate::kinds`].
    const KIND: u32;

    /// The `d`-tag prefix: a record with id `x` is stored at `d = PREFIX:x`.
    ///
    /// The prefix is what keeps a 64-hex id (a segment's sha256, say) out of
    /// a bare `d` tag: nostrdb stores a 64-hex tag value as a 32-byte id, and
    /// a string `d` lookup then never finds it.
    const PREFIX: &'static str;

    /// Add the record's fields to `tags`, one [`TagWriter::tag`] each. The
    /// `d` and `v` tags are the vault's; don't write them.
    fn write_tags<'b>(&self, tags: TagWriter<'b>) -> TagWriter<'b>;

    /// Read the record back from its note's tags, borrowing them, or `None`
    /// when a field it needs is missing. Ignore tags it doesn't know.
    fn from_tags(tags: &Tags<'a>) -> Option<Self>;
}

/// Builds a record's field tags for [`Record::write_tags`], and remembers the
/// first field whose value nostrdb would store as an id instead of a string.
pub struct TagWriter<'b> {
    note: NoteBuilder<'b>,
    hex_field: Option<&'static str>,
}

impl<'b> TagWriter<'b> {
    /// Add the tag `[name, values...]`.
    pub fn tag(mut self, name: &'static str, values: &[&str]) -> Self {
        if self.hex_field.is_none() && values.iter().any(|value| is_hex_id(value)) {
            self.hex_field = Some(name);
        }
        let mut note = self.note.start_tag().tag_str(name);
        for value in values {
            note = note.tag_str(value);
        }
        self.note = note;
        self
    }
}

/// The string value of the first `[name, value, ...]` tag in `tags`, borrowed
/// from the note. A record's [`Record::from_tags`] reads its fields with it.
pub fn tag_value<'a>(tags: &Tags<'a>, name: &str) -> Option<&'a str> {
    tags.iter()
        .find(|tag| tag.count() >= 2 && tag.get_str(0) == Some(name))
        .and_then(|tag| tag.get_str(1))
}

/// Whether nostrdb would store `value` as a 32-byte id: 64 hex characters.
fn is_hex_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// One record as read back, borrowing from the transaction: its id (the `d`
/// without the prefix), when it was written, and its body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored<'a, T> {
    /// The record's id, the `d` tag with `<prefix>:` taken off.
    pub id: &'a str,
    /// The revision's `created_at`, which the next write to it stamps past.
    pub created_at: u64,
    /// The record itself.
    pub body: T,
}

/// A record [`Vault::write`] stored.
#[derive(Clone, Debug)]
pub struct Written {
    /// The `created_at` it was stamped with. A caller writing the same
    /// record again before this one has landed passes it as `after`.
    pub created_at: u64,
    /// The sealed envelope as an `["EVENT", {…}]` frame, already ingested
    /// locally, for a caller with a relay to publish.
    pub frame: String,
}

/// Why a vault write stored nothing.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// A record id is empty or has whitespace in it.
    #[error("invalid record id {0:?}: it must be non-empty with no whitespace")]
    BadId(String),
    /// A field's value is 64 hex characters, which nostrdb would store as an
    /// id that the record's own string read never finds.
    #[error("field '{0}' holds a 64-hex value, which nostrdb stores as an id; prefix it")]
    HexValue(&'static str),
    /// Signing the record failed.
    #[error("couldn't build the record note")]
    Build,
    /// Serializing a note to JSON failed.
    #[error("note serialization failed: {0}")]
    Serialize(String),
    /// Sealing the record into its envelope failed.
    #[error("couldn't seal the record into the vault")]
    Wrap,
    /// nostrdb rejected the envelope.
    #[error("ndb ingest failed: {0}")]
    Ingest(String),
}

/// The account's vault: its signing key and the channel it seals into.
pub struct Vault {
    member: FullKeypair,
    keys: SnsKeys,
}

impl Vault {
    /// The vault of the account whose secret is `secret`, or `None` when the
    /// secret isn't a usable key.
    pub fn new(secret: &[u8; 32]) -> Option<Self> {
        Some(Self {
            member: FullKeypair::from_secret_bytes(secret)?,
            keys: sns::derive_sns_keys(&vault_root(secret))?,
        })
    }

    /// The account: the author of every record, and whose records reads ask
    /// for.
    pub fn author(&self) -> &Pubkey {
        &self.member.pubkey
    }

    /// The channel's team key, which signs every envelope. A relay sync of
    /// the vault asks for kind-1081 notes by this author ([`Self::sync_filter`]).
    pub fn team_pubkey(&self) -> &Pubkey {
        &self.keys.team_keypair.pubkey
    }

    /// The nostrdb filter for the vault's envelopes, which is all a relay
    /// sync of the vault moves.
    pub fn sync_filter(&self) -> Filter {
        Filter::new()
            .kinds([sns::SNS_ENVELOPE_KIND as u64])
            .authors([self.team_pubkey().bytes()])
            .build()
    }

    /// Register the vault root with `ndb`, so it unwraps the vault's
    /// envelopes, and unwrap any that arrived before it was registered.
    ///
    /// For a process with its own nostrdb, such as the CLI; the notedeck app
    /// hands the root to its host instead (`AppContext::register_team_root`).
    /// Roots don't survive a restart, so call it once per run, before reading.
    pub fn register(&self, ndb: &Ndb) {
        let root = vault_root(&self.member.secret_key.secret_bytes());
        if ndb.add_team_root(&root)
            && let Ok(txn) = Transaction::new(ndb)
        {
            ndb.process_sns(&txn);
        }
    }

    /// Write `body` as the newest revision of record `id`: stamped past the
    /// stored revision and `after` (pass 0, or the last [`Written`]'s
    /// `created_at` for this record), signed, sealed and ingested.
    ///
    /// nostrdb ingests on its own thread, so the record is readable once that
    /// lands, not necessarily on return.
    pub fn write<'r, T: Record<'r>>(
        &self,
        ndb: &Ndb,
        id: &str,
        body: &T,
        after: u64,
    ) -> Result<Written, VaultError> {
        let d = d_tag(T::PREFIX, id)?;
        // Any stored revision counts, even one this `T` can't read.
        let stored = Transaction::new(ndb)
            .ok()
            .and_then(|txn| Some(newest(ndb, &txn, self.author(), T::KIND, &d)?.created_at()))
            .unwrap_or(0);
        let created_at = next_stamp(unix_now(), stored, after);

        let note = NoteBuilder::new()
            .kind(T::KIND)
            .content("")
            .start_tag()
            .tag_str("d")
            .tag_str(&d)
            .start_tag()
            .tag_str("v")
            .tag_str(SCHEMA_VERSION);
        let fields = body.write_tags(TagWriter {
            note,
            hex_field: None,
        });
        if let Some(field) = fields.hex_field {
            return Err(VaultError::HexValue(field));
        }
        let secret = self.member.secret_key.secret_bytes();
        let rumor = fields
            .note
            .options(NoteBuildOptions::default())
            .created_at(created_at)
            .sign(&secret)
            .build()
            .ok_or(VaultError::Build)?;
        let rumor_json = rumor
            .json()
            .map_err(|err| VaultError::Serialize(err.to_string()))?;
        let envelope = sns::wrap_rumor(&self.keys, &self.member, &rumor_json, created_at)
            .ok_or(VaultError::Wrap)?;
        let frame = ClientMessage::event(&envelope)
            .and_then(|message| message.to_json())
            .map_err(|err| VaultError::Serialize(err.to_string()))?;
        ndb.process_event_with(&frame, IngestMetadata::new().client(true))
            .map_err(|err| VaultError::Ingest(err.to_string()))?;
        Ok(Written { created_at, frame })
    }
}

/// The newest revision of record `id` of type `T` that `author` wrote,
/// borrowed from `txn`, or `None` when there is none or it lacks a field `T`
/// needs.
pub fn latest<'a, T: Record<'a>>(
    ndb: &Ndb,
    txn: &'a Transaction,
    author: &Pubkey,
    id: &str,
) -> Option<Stored<'a, T>> {
    let d = d_tag(T::PREFIX, id).ok()?;
    stored(newest(ndb, txn, author, T::KIND, &d)?)
}

/// The newest revision of every record of type `T` that `author` wrote,
/// borrowed from `txn`, in id order. A note of `T`'s kind without `T`'s
/// prefix, or lacking a field `T` needs, is skipped.
pub fn latest_all<'a, T: Record<'a>>(
    ndb: &Ndb,
    txn: &'a Transaction,
    author: &Pubkey,
) -> Vec<Stored<'a, T>> {
    let mut all: Vec<Stored<'a, T>> =
        query_replaceable(ndb, txn, &[author_filter(author, T::KIND)])
            .into_iter()
            .filter_map(|key| stored(ndb.get_note_by_key(txn, key).ok()?))
            .collect();
    // query_replaceable hands its winners back in `HashMap` drain order.
    all.sort_unstable_by(|a, b| a.id.cmp(b.id));
    all
}

/// The newest revision of `author`'s note of `kind` at `d`, borrowed from
/// `txn`.
fn newest<'a>(
    ndb: &Ndb,
    txn: &'a Transaction,
    author: &Pubkey,
    kind: u32,
    d: &str,
) -> Option<Note<'a>> {
    // The predicate sees only each `d`'s newest revision, so it picks this
    // one's out of the author's notes of the kind.
    let key = query_replaceable_filtered(ndb, txn, &[author_filter(author, kind)], |note| {
        note_d(note) == Some(d)
    })
    .into_iter()
    .next()?;
    ndb.get_note_by_key(txn, key).ok()
}

/// `note` as a stored `T`, when its `d` has `T`'s prefix and its tags read.
fn stored<'a, T: Record<'a>>(note: Note<'a>) -> Option<Stored<'a, T>> {
    let id = note_d(&note)?.strip_prefix(T::PREFIX)?.strip_prefix(':')?;
    Some(Stored {
        id,
        created_at: note.created_at(),
        body: T::from_tags(&note.tags())?,
    })
}

/// Every note of `kind` that `author` signed. The `d` is checked in Rust: an
/// authors + kinds + tags filter scans the author's notes of the kind anyway.
fn author_filter(author: &Pubkey, kind: u32) -> Filter {
    Filter::new()
        .authors([author.bytes()])
        .kinds([kind as u64])
        .build()
}

/// `note`'s `d` tag, when it is a string.
fn note_d<'a>(note: &Note<'a>) -> Option<&'a str> {
    tag_value(&note.tags(), "d")
}

/// The `d` tag for record `id` under `prefix`: `<prefix>:<id>`.
fn d_tag(prefix: &str, id: &str) -> Result<String, VaultError> {
    if id.is_empty() || id.chars().any(char::is_whitespace) {
        return Err(VaultError::BadId(id.to_owned()));
    }
    Ok(format!("{prefix}:{id}"))
}

/// The wall clock in whole seconds, nostr's `created_at` unit.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::kinds;
    use crate::test_util::{INGEST_TIMEOUT as WAIT, TempNdb};

    /// A record shaped like the segment manifest the hex-trap test needs:
    /// its ids are 64-hex sha256s.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Probe<'a> {
        note: &'a str,
    }

    impl<'a> Record<'a> for Probe<'a> {
        const KIND: u32 = kinds::SEGMENT;
        const PREFIX: &'static str = "seg";

        fn write_tags<'b>(&self, tags: TagWriter<'b>) -> TagWriter<'b> {
            tags.tag("note", &[self.note])
        }

        fn from_tags(tags: &Tags<'a>) -> Option<Self> {
            Some(Probe {
                note: tag_value(tags, "note")?,
            })
        }
    }

    const SECRET: [u8; 32] = [3u8; 32];
    const OTHER_SECRET: [u8; 32] = [4u8; 32];
    const HEX: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    fn probe(note: &str) -> Probe<'_> {
        Probe { note }
    }

    /// Subscribe to the vault's [`Probe`] records, before anything that
    /// should land is ingested.
    fn subscribe(ndb: &Ndb) -> nostrdb::Subscription {
        ndb.subscribe(&[Filter::new().kinds([kinds::SEGMENT as u64]).build()])
            .unwrap()
    }

    #[tokio::test]
    async fn write_then_read_round_trips() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let sub = subscribe(ndb);

        let first = vault.write(ndb, "a", &probe("one"), 0).unwrap();
        // Same record again before the first has landed: `after` keeps the
        // second strictly newer.
        let second = vault
            .write(ndb, "a", &probe("two"), first.created_at)
            .unwrap();
        assert!(second.created_at > first.created_at);
        vault.write(ndb, "b", &probe("bee"), 0).unwrap();
        ndb.wait_for_all_notes_within(sub, 3, WAIT).await.unwrap();

        let txn = Transaction::new(ndb).unwrap();
        let a = latest::<Probe>(ndb, &txn, vault.author(), "a").unwrap();
        assert_eq!(a.body, probe("two"));
        assert_eq!(a.created_at, second.created_at);

        let all = latest_all::<Probe>(ndb, &txn, vault.author());
        let ids: Vec<&str> = all.iter().map(|s| s.id).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(all[0].body, probe("two"));

        assert!(latest::<Probe>(ndb, &txn, vault.author(), "c").is_none());

        // The rumor's content is empty: the record is its tags.
        let key = query_replaceable_filtered(
            ndb,
            &txn,
            &[author_filter(vault.author(), kinds::SEGMENT)],
            |note| note_d(note) == Some("seg:a"),
        )[0];
        let note = ndb.get_note_by_key(&txn, key).unwrap();
        assert_eq!(note.content(), "");
        assert_eq!(tag_value(&note.tags(), "v"), Some(SCHEMA_VERSION));
        let stranger = Vault::new(&OTHER_SECRET).unwrap();
        assert!(latest_all::<Probe>(ndb, &txn, stranger.author()).is_empty());
    }

    /// A record is its tags: empty content, `v` beside the fields, and a
    /// tag the reader doesn't know (a newer writer's) is ignored.
    #[test]
    fn records_are_tags_and_unknown_tags_are_ignored() {
        let newer = NoteBuilder::new()
            .kind(kinds::SEGMENT)
            .content("")
            .start_tag()
            .tag_str("d")
            .tag_str("seg:x")
            .start_tag()
            .tag_str("v")
            .tag_str("2")
            .start_tag()
            .tag_str("note")
            .tag_str("hi")
            .start_tag()
            .tag_str("added_later")
            .tag_str("1")
            .sign(&SECRET)
            .build()
            .unwrap();
        let read = stored::<Probe>(newer).unwrap();
        assert_eq!(read.id, "x");
        assert_eq!(read.body, probe("hi"));

        let missing_field = NoteBuilder::new()
            .kind(kinds::SEGMENT)
            .content("")
            .start_tag()
            .tag_str("d")
            .tag_str("seg:y")
            .sign(&SECRET)
            .build()
            .unwrap();
        assert!(stored::<Probe>(missing_field).is_none());

        let other_prefix = NoteBuilder::new()
            .kind(kinds::SEGMENT)
            .content("")
            .start_tag()
            .tag_str("d")
            .tag_str("ov:x")
            .start_tag()
            .tag_str("note")
            .tag_str("hi")
            .sign(&SECRET)
            .build()
            .unwrap();
        assert!(stored::<Probe>(other_prefix).is_none());
    }

    /// A 64-hex field value would be stored as an id its own string read
    /// misses, so the write refuses it instead.
    #[test]
    fn hex_field_values_are_refused() {
        let db = TempNdb::new();
        let vault = Vault::new(&SECRET).unwrap();
        assert!(matches!(
            vault.write(&db.ndb, "a", &probe(HEX), 0),
            Err(VaultError::HexValue("note"))
        ));
        assert!(is_hex_id(HEX));
        assert!(!is_hex_id(&HEX[1..]));
        assert!(!is_hex_id(&format!("seg:{HEX}")));
    }

    /// A `seg:<64hex>` record round-trips. The prefix is what keeps nostrdb
    /// from storing the `d` as a 32-byte id that a string lookup misses.
    #[tokio::test]
    async fn hex_ids_round_trip_behind_their_prefix() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let sub = subscribe(ndb);

        vault.write(ndb, HEX, &probe("segment"), 0).unwrap();
        ndb.wait_for_all_notes_within(sub, 1, WAIT).await.unwrap();

        let txn = Transaction::new(ndb).unwrap();
        let one = latest::<Probe>(ndb, &txn, vault.author(), HEX).unwrap();
        assert_eq!(one.body, probe("segment"));
        let all = latest_all::<Probe>(ndb, &txn, vault.author());
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, HEX);
    }

    /// A sealed record is opaque to a nostrdb that doesn't hold the vault
    /// root, and reads normally once the root is registered.
    #[tokio::test]
    async fn sealed_records_need_the_root() {
        let writer_db = TempNdb::new();
        let writer = &writer_db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(writer);
        let written = vault.write(writer, "a", &probe("secret"), 0).unwrap();
        assert!(!written.frame.contains("secret"), "plaintext on the wire");

        let reader_db = TempNdb::new();
        let reader = &reader_db.ndb;
        let envelopes = reader
            .subscribe(&[Filter::new().kinds([sns::SNS_ENVELOPE_KIND as u64]).build()])
            .unwrap();
        let records = subscribe(reader);
        // The frame is client-form (`["EVENT", {…}]`), as a publish sends it.
        reader
            .process_event_with(&written.frame, IngestMetadata::new().client(true))
            .unwrap();
        reader
            .wait_for_all_notes_within(envelopes, 1, WAIT)
            .await
            .unwrap();
        {
            let txn = Transaction::new(reader).unwrap();
            assert!(latest::<Probe>(reader, &txn, vault.author(), "a").is_none());
        }

        // Another account's root unwraps nothing either.
        Vault::new(&OTHER_SECRET).unwrap().register(reader);
        {
            let txn = Transaction::new(reader).unwrap();
            assert!(latest::<Probe>(reader, &txn, vault.author(), "a").is_none());
        }

        vault.register(reader);
        reader
            .wait_for_all_notes_within(records, 1, WAIT)
            .await
            .unwrap();
        let txn = Transaction::new(reader).unwrap();
        let read = latest::<Probe>(reader, &txn, vault.author(), "a").unwrap();
        assert_eq!(read.body, probe("secret"));
    }

    #[test]
    fn ids_must_be_non_empty_without_whitespace() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        for bad in ["", "a b", "a\n"] {
            assert!(matches!(
                vault.write(ndb, bad, &probe("x"), 0),
                Err(VaultError::BadId(_))
            ));
        }
    }
}
