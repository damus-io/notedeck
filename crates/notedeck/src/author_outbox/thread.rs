use enostr::{NormRelayUrl, NoteId, Pubkey, RelayUrlSource};
use hashbrown::HashSet;
use nostrdb::{Error, Ndb, NoteReply, Transaction};

/// Thread ancestry and routing information read in one NostrDB transaction.
///
/// Missing references remain in `note_ids` so a subscription can observe their
/// later arrival. Authors named by root/reply tags are routing hints only; they
/// do not restrict the authors of events requested from relays.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ThreadSnapshot {
    /// Every selected note, root, and reply parent encountered during traversal.
    pub(crate) note_ids: HashSet<NoteId>,
    /// Authors of available notes and authors claimed by their root/reply tags.
    pub(crate) authors: HashSet<Pubkey>,
    /// Referenced notes absent from this database snapshot.
    pub(crate) missing_ids: HashSet<NoteId>,
    /// Missing IDs with no claimed author on any encountered root/reply reference.
    pub(crate) missing_ids_without_author: HashSet<NoteId>,
    /// Allowed observed relays and NIP-10 root/reply relay hints.
    pub(crate) relays: HashSet<NormRelayUrl>,
}

impl ThreadSnapshot {
    /// Traverse available root/reply ancestry without recursion or UI work.
    ///
    /// Each ID is visited once, including missing IDs and cyclic references.
    /// Mentions and other unrelated tags do not contribute routing information.
    #[profiling::function]
    pub(crate) fn load(ndb: &Ndb, seeds: &HashSet<NoteId>) -> Result<Self, Error> {
        let txn = Transaction::new(ndb)?;
        let mut snapshot = Self::default();
        let mut ids_with_claimed_authors = HashSet::new();
        let mut pending = seeds.iter().copied().collect::<Vec<_>>();
        while let Some(id) = pending.pop() {
            if !snapshot.note_ids.insert(id) {
                continue;
            }

            let note = match ndb.get_note_by_id(&txn, id.bytes()) {
                Ok(note) => note,
                Err(Error::NotFound) => {
                    snapshot.missing_ids.insert(id);
                    continue;
                }
                Err(err) => return Err(err),
            };
            snapshot.authors.insert(Pubkey::new(*note.pubkey()));
            for relay in note.relays(&txn) {
                snapshot.retain_relay(relay);
            }

            let reply = NoteReply::new(note.tags());
            for reference in [reply.root(), reply.reply()].into_iter().flatten() {
                pending.push(NoteId::new(*reference.id));
                if let Some(relay) = reference.relay {
                    snapshot.retain_relay(relay);
                }
                if let Some(author) = note
                    .tags()
                    .into_iter()
                    .nth(usize::from(reference.index))
                    .and_then(|tag| tag.get_id(4))
                {
                    snapshot.authors.insert(Pubkey::new(*author));
                    ids_with_claimed_authors.insert(NoteId::new(*reference.id));
                }
            }
        }
        // Another reference may name an author after this missing ID was visited.
        snapshot.missing_ids_without_author = snapshot
            .missing_ids
            .difference(&ids_with_claimed_authors)
            .copied()
            .collect();
        Ok(snapshot)
    }

    /// Keep normalized relay hints under the existing remote-advertised policy.
    fn retain_relay(&mut self, relay: &str) {
        let Ok(relay) = NormRelayUrl::new(relay) else {
            return;
        };
        if relay.allowed_for_source(RelayUrlSource::RemoteAdvertised) {
            self.relays.insert(relay);
        }
    }
}

#[test]
fn thread_snapshot_uses_all_references_to_identify_missing_ids_without_authors() {
    use nostrdb::{Config, NoteBuilder};
    use std::time::{Duration, Instant};

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let ndb = Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
    let first = NoteId::new([11; 32]);
    let second = NoteId::new([12; 32]);
    let unclaimed = NoteId::new([13; 32]);
    let missing_seed = NoteId::new([14; 32]);
    let claimed_author = Pubkey::new([2; 32]);
    let mut seeds = HashSet::from([missing_seed]);
    let mut inserted = Vec::new();
    for (created_at, root_author, reply_author) in [(1, true, false), (2, false, true)] {
        let mut builder = NoteBuilder::new()
            .kind(1)
            .created_at(created_at)
            .content("references share missing ancestors")
            .start_tag()
            .tag_str("e")
            .tag_id(first.bytes())
            .tag_str("")
            .tag_str("root");
        if root_author {
            builder = builder.tag_id(claimed_author.bytes());
        }
        builder = builder
            .start_tag()
            .tag_str("e")
            .tag_id(second.bytes())
            .tag_str("")
            .tag_str("reply");
        if reply_author {
            builder = builder.tag_id(claimed_author.bytes());
        }
        let note = builder.sign(&[1; 32]).build().expect("note");
        let id = NoteId::new(*note.id());
        seeds.insert(id);
        inserted.push(id);
        ndb.process_client_event(&note.json().expect("json"))
            .expect("ingest note");
    }
    let unclaimed_ref = NoteBuilder::new()
        .kind(1)
        .created_at(3)
        .content("ancestor without claimed author")
        .start_tag()
        .tag_str("e")
        .tag_id(unclaimed.bytes())
        .tag_str("")
        .tag_str("reply")
        .sign(&[1; 32])
        .build()
        .expect("note");
    let unclaimed_ref_id = NoteId::new(*unclaimed_ref.id());
    seeds.insert(unclaimed_ref_id);
    inserted.push(unclaimed_ref_id);
    ndb.process_client_event(&unclaimed_ref.json().expect("json"))
        .expect("ingest note");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let txn = Transaction::new(&ndb).expect("txn");
        if inserted
            .iter()
            .all(|id| ndb.get_note_by_id(&txn, id.bytes()).is_ok())
        {
            break;
        }
        assert!(Instant::now() < deadline, "notes were not ingested");
        std::thread::sleep(Duration::from_millis(10));
    }

    let snapshot = ThreadSnapshot::load(&ndb, &seeds).expect("snapshot");
    assert_eq!(
        snapshot.missing_ids,
        HashSet::from([first, second, unclaimed, missing_seed])
    );
    assert_eq!(
        snapshot.missing_ids_without_author,
        HashSet::from([unclaimed, missing_seed])
    );
}

#[test]
fn thread_snapshot_retains_missing_parents_and_their_routing_hints() {
    use enostr::{NormRelayUrl, NoteId, Pubkey};
    use hashbrown::HashSet;
    use nostrdb::{Config, IngestMetadata, Ndb, NoteBuilder, Transaction};
    use std::time::{Duration, Instant};

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let ndb = Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
    let root_id = NoteId::new([11; 32]);
    let parent_id = NoteId::new([12; 32]);
    let root_author = Pubkey::new([2; 32]);
    let parent_author = Pubkey::new([3; 32]);
    let selected = NoteBuilder::new()
        .kind(1)
        .created_at(1)
        .content("selected reply")
        .start_tag()
        .tag_str("e")
        .tag_id(root_id.bytes())
        .tag_str("wss://root.example.com")
        .tag_str("root")
        .tag_id(root_author.bytes())
        .start_tag()
        .tag_str("e")
        .tag_id(parent_id.bytes())
        .tag_str("wss://parent.example.com")
        .tag_str("reply")
        .tag_id(parent_author.bytes())
        .sign(&[1; 32])
        .build()
        .expect("selected note");
    ndb.process_event_with(
        &selected.json().expect("json"),
        IngestMetadata::new()
            .client(true)
            .relay("wss://observed.example.com"),
    )
    .expect("ingest selected note");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let txn = Transaction::new(&ndb).expect("txn");
        if ndb.get_note_by_id(&txn, selected.id()).is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "selected note was not ingested");
        std::thread::sleep(Duration::from_millis(10));
    }

    let selected_id = NoteId::new(*selected.id());
    let snapshot = ThreadSnapshot::load(&ndb, &HashSet::from([selected_id])).expect("snapshot");
    assert_eq!(
        snapshot.note_ids,
        HashSet::from([selected_id, root_id, parent_id])
    );
    assert_eq!(snapshot.missing_ids, HashSet::from([root_id, parent_id]));
    assert_eq!(
        snapshot.authors,
        HashSet::from([Pubkey::new(*selected.pubkey()), root_author, parent_author])
    );
    assert_eq!(
        snapshot.relays,
        HashSet::from([
            NormRelayUrl::new("wss://root.example.com").expect("relay"),
            NormRelayUrl::new("wss://parent.example.com").expect("relay"),
            NormRelayUrl::new("wss://observed.example.com").expect("relay"),
        ])
    );
}
