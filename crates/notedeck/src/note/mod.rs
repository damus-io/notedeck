mod action;
mod context;
pub mod publish;

pub use action::{NoteAction, ReactAction, ScrollInfo, ZapAction, ZapTargetAmount};
pub use context::{BroadcastContext, ContextSelection, NoteContextSelection};
pub use publish::{
    builder_from_note, send_mute_event, send_people_list_event, send_report_event,
    send_unmute_event, ReportTarget, ReportType,
};

use crate::jobs::MediaJobSender;
use crate::nip05::Nip05Cache;
use crate::sound::SoundManager;
use crate::Accounts;
use crate::GlobalWallet;
use crate::Localization;
use crate::UnknownIds;
use crate::{notecache::NoteCache, zaps::Zaps, Images};
use enostr::NoteId;
use nostrdb::{Ndb, Note, NoteKey, QueryResult, Transaction};
use std::borrow::Borrow;
use std::cmp::Ordering;
use std::fmt;

/// Aggregates dependencies to reduce the number of parameters
/// passed to inner UI elements, minimizing prop drilling.
pub struct NoteContext<'d> {
    pub ndb: &'d Ndb,
    pub accounts: &'d Accounts,
    pub global_wallet: &'d GlobalWallet,
    pub i18n: &'d mut Localization,
    pub img_cache: &'d mut Images,
    pub note_cache: &'d mut NoteCache,
    pub zaps: &'d mut Zaps,
    pub jobs: &'d MediaJobSender,
    pub unknown_ids: &'d mut UnknownIds,
    pub nip05_cache: &'d mut Nip05Cache,
    pub clipboard: &'d mut egui_winit::clipboard::Clipboard,
    pub sound: &'d SoundManager,
}

#[derive(Debug, Eq, PartialEq, Copy, Clone, Hash)]
pub struct NoteRef {
    pub key: NoteKey,
    pub created_at: u64,
}

#[derive(Clone, Copy, Eq, PartialEq, Hash)]
pub struct RootNoteIdBuf([u8; 32]);

impl fmt::Debug for RootNoteIdBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RootNoteIdBuf({})", self.hex())
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Debug, Hash)]
pub struct RootNoteId<'a>(&'a [u8; 32]);

impl RootNoteIdBuf {
    pub fn to_note_id(self) -> NoteId {
        NoteId::new(self.0)
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn new(
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        id: &[u8; 32],
    ) -> Result<RootNoteIdBuf, RootIdError> {
        root_note_id_from_selected_id(ndb, note_cache, txn, id).map(|rnid| Self(*rnid.bytes()))
    }

    pub fn hex(&self) -> String {
        hex::encode(self.bytes())
    }

    pub fn new_unsafe(id: [u8; 32]) -> Self {
        Self(id)
    }

    pub fn borrow(&self) -> RootNoteId<'_> {
        RootNoteId(self.bytes())
    }
}

impl<'a> RootNoteId<'a> {
    pub fn to_note_id(self) -> NoteId {
        NoteId::new(*self.0)
    }

    pub fn bytes(&self) -> &[u8; 32] {
        self.0
    }

    pub fn hex(&self) -> String {
        hex::encode(self.bytes())
    }

    pub fn to_owned(&self) -> RootNoteIdBuf {
        RootNoteIdBuf::new_unsafe(*self.bytes())
    }

    pub fn new(
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &'a Transaction,
        id: &'a [u8; 32],
    ) -> Result<RootNoteId<'a>, RootIdError> {
        root_note_id_from_selected_id(ndb, note_cache, txn, id)
    }

    pub fn new_unsafe(id: &'a [u8; 32]) -> Self {
        Self(id)
    }
}

impl Borrow<[u8; 32]> for RootNoteIdBuf {
    fn borrow(&self) -> &[u8; 32] {
        &self.0
    }
}

impl Borrow<[u8; 32]> for RootNoteId<'_> {
    fn borrow(&self) -> &[u8; 32] {
        self.0
    }
}

impl NoteRef {
    pub fn new(key: NoteKey, created_at: u64) -> Self {
        NoteRef { key, created_at }
    }

    pub fn from_note(note: &Note<'_>) -> Self {
        let created_at = note.created_at();
        let key = note.key().expect("todo: implement NoteBuf");
        NoteRef::new(key, created_at)
    }

    pub fn from_query_result(qr: QueryResult<'_>) -> Self {
        NoteRef {
            key: qr.note_key,
            created_at: qr.note.created_at(),
        }
    }
}

impl Ord for NoteRef {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.created_at.cmp(&other.created_at) {
            Ordering::Equal => self.key.cmp(&other.key),
            Ordering::Less => Ordering::Greater,
            Ordering::Greater => Ordering::Less,
        }
    }
}

impl PartialOrd for NoteRef {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Copy, Clone)]
pub enum RootIdError {
    NoteNotFound,
    NoRootId,
}

pub fn root_note_id_from_selected_id<'txn, 'a>(
    ndb: &Ndb,
    note_cache: &mut NoteCache,
    txn: &'txn Transaction,
    selected_note_id: &'a [u8; 32],
) -> Result<RootNoteId<'txn>, RootIdError>
where
    'a: 'txn,
{
    let selected_note_key = if let Ok(key) = ndb.get_notekey_by_id(txn, selected_note_id) {
        key
    } else {
        return Err(RootIdError::NoteNotFound);
    };

    let note = if let Ok(note) = ndb.get_note_by_key(txn, selected_note_key) {
        note
    } else {
        return Err(RootIdError::NoteNotFound);
    };

    note_cache
        .cached_note_or_insert(selected_note_key, &note)
        .reply
        .borrow(note.tags())
        .root()
        .map_or_else(
            || Ok(RootNoteId::new_unsafe(selected_note_id)),
            |rnid| Ok(RootNoteId::new_unsafe(rnid.id)),
        )
}

pub fn event_tag<'a>(ev: &nostrdb::Note<'a>, name: &str) -> Option<&'a str> {
    ev.tags().iter().find_map(|tag| {
        if tag.count() < 2 {
            return None;
        }

        let cur_name = tag.get_str(0)?;

        if cur_name != name {
            return None;
        }

        tag.get_str(1)
    })
}

/// Temporary way of checking whether a user has sent a reaction.
/// Should be replaced with nostrdb metadata
pub fn reaction_sent_id(sender_pk: &enostr::Pubkey, note_reacted_to: &[u8; 32]) -> egui::Id {
    egui::Id::new(("sent-reaction-id", note_reacted_to, sender_pk))
}

/// Count the number of hashtags in a note by examining its tags
pub fn count_hashtags(note: &Note) -> usize {
    let mut count = 0;

    for tag in note.tags() {
        // Early continue if not enough elements
        if tag.count() < 2 {
            continue;
        }

        // Check if this is a hashtag tag (type "t")
        let Some("t") = tag.get_unchecked(0).variant().str() else {
            continue;
        };

        count += 1;
    }

    count
}

pub fn get_p_tags<'a>(note: &Note<'a>) -> Vec<&'a [u8; 32]> {
    let mut items = Vec::new();
    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }

        if tag.get_str(0) != Some("p") {
            continue;
        }

        let Some(item) = tag.get_id(1) else {
            continue;
        };

        items.push(item);
    }

    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::test_config;
    use enostr::FullKeypair;
    use nostrdb::NoteBuilder;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    fn test_note(
        account: &FullKeypair,
        content: &str,
        created_at: u64,
        tags: &[&[&str]],
    ) -> Note<'static> {
        let mut builder = NoteBuilder::new()
            .kind(1)
            .content(content)
            .created_at(created_at);

        for tag in tags {
            builder = builder.start_tag();
            for component in *tag {
                builder = builder.tag_str(component);
            }
        }

        builder
            .sign(&account.secret_key.secret_bytes())
            .build()
            .expect("note")
    }

    fn setup_ndb() -> (TempDir, Ndb) {
        let tmp = TempDir::new().expect("tmpdir");
        let ndb = Ndb::new(tmp.path().to_str().expect("db path"), &test_config()).expect("ndb");
        (tmp, ndb)
    }

    fn ingest(ndb: &Ndb, note: &Note<'_>) {
        ndb.process_client_event(&note.json().expect("note json"))
            .expect("ingest note");

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let txn = Transaction::new(ndb).expect("txn");
            if ndb.get_note_by_id(&txn, note.id()).is_ok() {
                break;
            }

            assert!(
                Instant::now() < deadline,
                "timed out waiting for note import"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn root_note_id_returns_selected_for_root_note() {
        let (_tmp, ndb) = setup_ndb();
        let account = FullKeypair::generate();
        let root = test_note(&account, "root", 1_700_000_000, &[]);
        ingest(&ndb, &root);

        let txn = Transaction::new(&ndb).expect("txn");
        let mut note_cache = NoteCache::default();
        let resolved =
            root_note_id_from_selected_id(&ndb, &mut note_cache, &txn, root.id()).expect("root id");

        assert_eq!(resolved.bytes(), root.id());
    }

    #[test]
    fn root_note_id_resolves_marked_root_for_direct_reply() {
        let (_tmp, ndb) = setup_ndb();
        let root_author = FullKeypair::generate();
        let reply_author = FullKeypair::generate();
        let root = test_note(&root_author, "root", 1_700_000_000, &[]);
        let root_hex = hex::encode(root.id());
        let direct_reply = test_note(
            &reply_author,
            "direct reply",
            1_700_000_001,
            &[&[
                "e",
                root_hex.as_str(),
                "wss://relay.example",
                "root",
                &root_author.pubkey.hex(),
            ]],
        );
        ingest(&ndb, &root);
        ingest(&ndb, &direct_reply);

        let txn = Transaction::new(&ndb).expect("txn");
        let mut note_cache = NoteCache::default();
        let resolved =
            root_note_id_from_selected_id(&ndb, &mut note_cache, &txn, direct_reply.id())
                .expect("root id");

        assert_eq!(resolved.bytes(), root.id());
    }

    #[test]
    fn root_note_id_resolves_deprecated_single_e_reply() {
        let (_tmp, ndb) = setup_ndb();
        let root_author = FullKeypair::generate();
        let reply_author = FullKeypair::generate();
        let root = test_note(&root_author, "root", 1_700_000_000, &[]);
        let root_hex = hex::encode(root.id());
        let direct_reply = test_note(
            &reply_author,
            "deprecated direct reply",
            1_700_000_001,
            &[&["e", root_hex.as_str()]],
        );
        ingest(&ndb, &root);
        ingest(&ndb, &direct_reply);

        let txn = Transaction::new(&ndb).expect("txn");
        let mut note_cache = NoteCache::default();
        let resolved =
            root_note_id_from_selected_id(&ndb, &mut note_cache, &txn, direct_reply.id())
                .expect("root id");

        assert_eq!(resolved.bytes(), root.id());
    }

    #[test]
    fn root_note_id_resolves_root_for_nested_reply() {
        let (_tmp, ndb) = setup_ndb();
        let root_author = FullKeypair::generate();
        let reply_author = FullKeypair::generate();
        let nested_author = FullKeypair::generate();
        let root = test_note(&root_author, "root", 1_700_000_000, &[]);
        let root_hex = hex::encode(root.id());
        let direct_reply = test_note(
            &reply_author,
            "direct reply",
            1_700_000_001,
            &[&["e", root_hex.as_str(), "", "root"]],
        );
        let direct_reply_hex = hex::encode(direct_reply.id());
        let nested_reply = test_note(
            &nested_author,
            "nested reply",
            1_700_000_002,
            &[
                &["e", root_hex.as_str(), "", "root"],
                &["e", direct_reply_hex.as_str(), "", "reply"],
            ],
        );
        ingest(&ndb, &root);
        ingest(&ndb, &direct_reply);
        ingest(&ndb, &nested_reply);

        let txn = Transaction::new(&ndb).expect("txn");
        let mut note_cache = NoteCache::default();
        let resolved =
            root_note_id_from_selected_id(&ndb, &mut note_cache, &txn, nested_reply.id())
                .expect("root id");

        assert_eq!(resolved.bytes(), root.id());
    }

    #[test]
    fn root_note_id_returns_note_not_found_for_unknown_id() {
        let (_tmp, ndb) = setup_ndb();
        let txn = Transaction::new(&ndb).expect("txn");
        let mut note_cache = NoteCache::default();
        let missing = [7u8; 32];

        let err = root_note_id_from_selected_id(&ndb, &mut note_cache, &txn, &missing).unwrap_err();

        assert!(matches!(err, RootIdError::NoteNotFound));
    }
}
