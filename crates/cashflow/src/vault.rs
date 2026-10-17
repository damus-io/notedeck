//! The account's private cashflow vault: small typed records over nostr.
//!
//! Everything cashflow *decides* — settings now; rules, overrides, splits,
//! transfer links, segment manifests and account metadata in later cards — is
//! one [`Record`] each: a parameterized-replaceable note of a kind from
//! [`crate::kinds`], `d = <prefix>:<id>`, JSON content with a schema version.
//! Bulk transaction data never goes here; it lives in journal segments.
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
//! Reads and the write stamp are [`nostrdb_net::private_note`]'s: the newest
//! revision of a `d` by `(created_at, id)`, and a write stamped strictly past
//! both the stored revision and the caller's own last write. Only the wrapping
//! differs (SNS here, PNS there), so the read helpers are shared and the write
//! is ours.

use nostrdb::{IngestMetadata, Ndb, NoteBuildOptions, NoteBuilder, Transaction};
use nostrdb_net::private_note::{self, Slot};
use nostrdb_net::sns::{self, SnsKeys};
use nostrdb_net::{ClientMessage, FullKeypair, Pubkey};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// The label the vault root is derived under: one vault per account.
const VAULT_LABEL: &str = "cashflow";

/// The schema version every record is written with, as the content's `v`.
/// Readers ignore fields they don't know, so adding a field doesn't bump it;
/// changing what an existing field means does.
pub const SCHEMA_VERSION: u32 = 1;

/// The account's vault `team_root`, derived from its `secret`. The same on
/// every device that holds the secret.
pub fn vault_root(secret: &[u8; 32]) -> [u8; 32] {
    sns::derive_board_root(secret, VAULT_LABEL)
}

/// A typed vault record: the body of one note, and where it is stored.
///
/// The body is a struct serialized as the note's JSON content, with
/// `"v": SCHEMA_VERSION` added beside its fields. Don't use
/// `#[serde(deny_unknown_fields)]`: a newer writer's extra fields must read
/// as nothing on an older reader, not as an unreadable record.
pub trait Record: Serialize + DeserializeOwned {
    /// The addressable kind it is stored under, from [`crate::kinds`].
    const KIND: u32;

    /// The `d`-tag prefix: a record with id `x` is stored at `d = PREFIX:x`.
    ///
    /// The prefix is what keeps a 64-hex id (a segment's sha256, say) out of
    /// a bare `d` tag: nostrdb stores a 64-hex tag value as a 32-byte id, and
    /// a string `d` lookup then never finds it.
    const PREFIX: &'static str;
}

/// One record as read back: its id (the `d` without the prefix), when it was
/// written, and its body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored<T> {
    /// The record's id, the `d` tag with `<prefix>:` taken off.
    pub id: String,
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
    /// The body didn't serialize to a JSON object.
    #[error("record body must serialize to a JSON object: {0}")]
    Body(String),
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
    pub fn sync_filter(&self) -> nostrdb::Filter {
        nostrdb::Filter::new()
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
    pub fn write<T: Record>(
        &self,
        ndb: &Ndb,
        id: &str,
        body: &T,
        after: u64,
    ) -> Result<Written, VaultError> {
        let d = d_tag::<T>(id)?;
        let content = content_json(body)?;
        let slot = Slot {
            author: self.author(),
            kind: T::KIND,
            d: &d,
        };
        let stored = Transaction::new(ndb)
            .ok()
            .and_then(|txn| private_note::latest(ndb, &txn, slot))
            .map_or(0, |revision| revision.created_at);
        let created_at = private_note::next_stamp(unix_now(), stored, after);

        let secret = self.member.secret_key.secret_bytes();
        let rumor = NoteBuilder::new()
            .kind(T::KIND)
            .content(&content)
            .start_tag()
            .tag_str("d")
            .tag_str(&d)
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

/// The newest revision of record `id` of type `T` that `author` wrote, or
/// `None` when there is none or its content doesn't read as a `T`.
pub fn latest<T: Record>(
    ndb: &Ndb,
    txn: &Transaction,
    author: &Pubkey,
    id: &str,
) -> Option<Stored<T>> {
    let d = d_tag::<T>(id).ok()?;
    let slot = Slot {
        author,
        kind: T::KIND,
        d: &d,
    };
    let revision = private_note::latest(ndb, txn, slot)?;
    Some(Stored {
        id: id.to_owned(),
        created_at: revision.created_at,
        body: serde_json::from_str(&revision.content).ok()?,
    })
}

/// The newest revision of every record of type `T` that `author` wrote, in
/// id order. A note of `T`'s kind without `T`'s prefix, or whose content
/// doesn't read as a `T`, is skipped.
pub fn latest_all<T: Record>(ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> Vec<Stored<T>> {
    private_note::latest_per_d(ndb, txn, author, T::KIND)
        .into_iter()
        .filter_map(|slot| {
            let id = slot.d.strip_prefix(T::PREFIX)?.strip_prefix(':')?;
            Some(Stored {
                id: id.to_owned(),
                created_at: slot.revision.created_at,
                body: serde_json::from_str(&slot.revision.content).ok()?,
            })
        })
        .collect()
}

/// `T`'s `d` tag for record `id`: `<prefix>:<id>`.
fn d_tag<T: Record>(id: &str) -> Result<String, VaultError> {
    if id.is_empty() || id.chars().any(char::is_whitespace) {
        return Err(VaultError::BadId(id.to_owned()));
    }
    Ok(format!("{}:{id}", T::PREFIX))
}

/// `body` as note content: its JSON object with `"v"` added.
fn content_json<T: Serialize>(body: &T) -> Result<String, VaultError> {
    let value = serde_json::to_value(body).map_err(|err| VaultError::Body(err.to_string()))?;
    let serde_json::Value::Object(mut fields) = value else {
        return Err(VaultError::Body(format!("got {value}")));
    };
    fields.insert("v".to_owned(), SCHEMA_VERSION.into());
    Ok(serde_json::Value::Object(fields).to_string())
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

    use nostrdb::Filter;
    use serde::Deserialize;

    use crate::kinds;
    use crate::test_util::{INGEST_TIMEOUT as WAIT, TempNdb};

    /// A record shaped like the segment manifest the hex-trap test needs:
    /// its ids are 64-hex sha256s.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Probe {
        note: String,
    }

    impl Record for Probe {
        const KIND: u32 = kinds::SEGMENT;
        const PREFIX: &'static str = "seg";
    }

    const SECRET: [u8; 32] = [3u8; 32];
    const OTHER_SECRET: [u8; 32] = [4u8; 32];
    const HEX: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    fn probe(note: &str) -> Probe {
        Probe {
            note: note.to_owned(),
        }
    }

    /// Subscribe to the vault's records of `T`'s kind, before anything that
    /// should land is ingested.
    fn subscribe<T: Record>(ndb: &Ndb) -> nostrdb::Subscription {
        ndb.subscribe(&[Filter::new().kinds([T::KIND as u64]).build()])
            .unwrap()
    }

    #[tokio::test]
    async fn write_then_read_round_trips() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let sub = subscribe::<Probe>(ndb);

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
        let ids: Vec<&str> = all.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(all[0].body, probe("two"));

        assert!(latest::<Probe>(ndb, &txn, vault.author(), "c").is_none());
        let stranger = Vault::new(&OTHER_SECRET).unwrap();
        assert!(latest_all::<Probe>(ndb, &txn, stranger.author()).is_empty());
    }

    /// The content carries `"v"`, and a field the reader doesn't know is
    /// ignored rather than making the record unreadable.
    #[test]
    fn content_is_versioned_and_extra_fields_are_ignored() {
        let content = content_json(&probe("x")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(value["v"], SCHEMA_VERSION);
        assert_eq!(value["note"], "x");

        let newer = r#"{"v":2,"note":"x","added_later":[1,2]}"#;
        assert_eq!(serde_json::from_str::<Probe>(newer).unwrap(), probe("x"));
    }

    /// A `seg:<64hex>` record round-trips. The prefix is what keeps nostrdb
    /// from storing the `d` as a 32-byte id that a string lookup misses.
    #[tokio::test]
    async fn hex_ids_round_trip_behind_their_prefix() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let sub = subscribe::<Probe>(ndb);

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
        let records = subscribe::<Probe>(reader);
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
