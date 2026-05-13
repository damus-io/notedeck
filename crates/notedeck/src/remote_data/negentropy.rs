use crate::jobs::JobSpawner;
use enostr::EventChecker;
use enostr::NegSetProvider;
use enostr::NoteId;
use hashbrown::HashSet;
use negentropy::{Id, NegentropyStorageVector};
use nostrdb::{Filter, Ndb, SendFilter, Transaction};

const NEGENTROPY_INFINITY_TIMESTAMP: u64 = u64::MAX;

/// Background worker that builds local negentropy sets from `nostrdb`.
pub(super) struct FullHistoryNegSetProvider {
    ndb: Ndb,
    job_spawner: JobSpawner,
}

impl FullHistoryNegSetProvider {
    /// Build the negentropy provider backed by `nostrdb` and `JobSpawner`.
    pub(super) fn new(ndb: Ndb, job_spawner: JobSpawner) -> Self {
        Self { ndb, job_spawner }
    }
}

impl NegSetProvider for FullHistoryNegSetProvider {
    fn provide(&self, filter: &Filter) -> tokio::sync::oneshot::Receiver<NegentropyStorageVector> {
        let ndb = self.ndb.clone();
        let filter_elements = filter.num_elements();
        let filter = SendFilter::try_clone_from_filter(filter);
        if filter.is_none() {
            tracing::warn!(
                filter_elements,
                "full-history local negentropy set build skipped: filter is not sendable"
            );
        }
        self.job_spawner.schedule_receivable(move || {
            let Some(filter) = filter else {
                return NegentropyStorageVector::new();
            };

            build_negentropy_storage(&ndb, filter.as_filter())
        })
    }
}

/// Cheap synchronous missing-id filter backed by `nostrdb`.
pub(super) struct NdbEventChecker {
    pub(super) ndb: Ndb,
}

impl EventChecker for NdbEventChecker {
    fn retain_missing(&self, ids: &mut HashSet<NoteId>) {
        let Ok(txn) = Transaction::new(&self.ndb) else {
            tracing::warn!("full-history local presence check skipped: failed to open txn");
            return;
        };

        ids.retain(|id| {
            let missing = self.ndb.get_note_by_id(&txn, id.bytes()).is_err();
            missing
        });
    }
}

/// Build a sealed negentropy storage vector for one local filter snapshot.
fn build_negentropy_storage(ndb: &Ndb, filter: &Filter) -> NegentropyStorageVector {
    let mut storage = NegentropyStorageVector::new();
    let Ok(txn) = Transaction::new(ndb) else {
        tracing::warn!("full-history local negentropy set build skipped: failed to open txn");
        return storage;
    };
    let result = ndb.fold(
        &txn,
        std::slice::from_ref(filter),
        &mut storage,
        |storage, note| {
            let created_at = note.created_at();
            let id = Id::from_byte_array(*note.id());
            insert_negentropy_record(storage, created_at, id);
            storage
        },
    );

    match result {
        Ok(_) => {
            let _ = storage.seal();
        }
        Err(err) => {
            tracing::warn!("full-history local negentropy set build failed: {err:?}");
        }
    }

    storage
}

/// Insert one NIP-77 record unless its timestamp is reserved as infinity.
fn insert_negentropy_record(storage: &mut NegentropyStorageVector, created_at: u64, id: Id) {
    if created_at == NEGENTROPY_INFINITY_TIMESTAMP {
        return;
    }

    let _ = storage.insert(created_at, id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::JobPool;
    use enostr::FullKeypair;
    use hashbrown::HashSet;
    use negentropy::{Item, NegentropyStorageBase};
    use nostrdb::{Config, IngestMetadata, NoteBuilder};
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    /// Creates one temporary `nostrdb` instance for negentropy adapter tests.
    fn test_ndb() -> (TempDir, Ndb) {
        let tmp = TempDir::new().expect("tmp dir");
        let ndb = Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
        (tmp, ndb)
    }

    /// Builds and ingests one signed text note into `nostrdb`.
    fn ingest_text_note(ndb: &Ndb, kind: u32, content: &str, created_at: u64) -> NoteId {
        let keypair = FullKeypair::generate();
        let note = NoteBuilder::new()
            .kind(kind)
            .content(content)
            .created_at(created_at)
            .sign(&keypair.secret_key.secret_bytes())
            .build()
            .expect("signed note");
        let json = note.json().expect("note json");
        ndb.process_event_with(&json, IngestMetadata::new().client(true))
            .expect("ingest client event");
        NoteId::new(*note.id())
    }

    /// Waits until one freshly queued note becomes queryable through
    /// `nostrdb::Transaction`.
    fn wait_for_note(ndb: &Ndb, note_id: NoteId) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(txn) = Transaction::new(ndb) {
                if ndb.get_note_by_id(&txn, note_id.bytes()).is_ok() {
                    return;
                }
            }

            assert!(
                Instant::now() < deadline,
                "timed out waiting for local note"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The real nostrdb-backed provider should build a sealed local negentropy
    /// set containing only notes matched by the filter.
    #[tokio::test]
    async fn provider_builds_storage_from_matching_local_notes_only() {
        let (_tmp, ndb) = test_ndb();
        let pool = JobPool::default();
        let provider = FullHistoryNegSetProvider::new(ndb.clone(), pool.spawner());
        let matching_first = ingest_text_note(&ndb, 1, "first", 1_776_000_010);
        let _non_matching = ingest_text_note(&ndb, 7, "skip", 1_776_000_020);
        let matching_second = ingest_text_note(&ndb, 1, "second", 1_776_000_030);
        let filter = Filter::new().kinds(vec![1]).limit(10).build();
        wait_for_note(&ndb, matching_first);
        wait_for_note(&ndb, matching_second);

        let storage = provider
            .provide(&filter)
            .await
            .expect("negentropy storage result");

        assert_eq!(storage.size().expect("sealed storage size"), 2);
        assert_eq!(
            storage.get_item(0).expect("first storage item"),
            Some(Item::with_timestamp_and_id(
                1_776_000_010,
                Id::from_byte_array(*matching_first.bytes()),
            )),
        );
        assert_eq!(
            storage.get_item(1).expect("second storage item"),
            Some(Item::with_timestamp_and_id(
                1_776_000_030,
                Id::from_byte_array(*matching_second.bytes()),
            )),
        );
    }

    /// Large filters should use nostrdb's sendable clone path, not the default
    /// JSON buffer used by `Filter::json`.
    #[tokio::test]
    async fn provider_builds_storage_for_filter_larger_than_json_buffer() {
        let (_tmp, ndb) = test_ndb();
        let pool = JobPool::default();
        let provider = FullHistoryNegSetProvider::new(ndb.clone(), pool.spawner());
        let matching = ingest_text_note(&ndb, 1, "large filter match", 1_776_000_060);
        wait_for_note(&ndb, matching);

        let mut ids = vec![*matching.bytes()];
        let mut index = 0u64;
        while ids.len() < 18_000 {
            let mut id = [0u8; 32];
            id[..8].copy_from_slice(&index.to_be_bytes());
            if id != *matching.bytes() {
                ids.push(id);
            }
            index += 1;
        }
        let filter = Filter::new_with_capacity(512).ids(ids.iter()).build();

        assert!(
            filter.json().is_err(),
            "test filter should exceed Filter::json default buffer"
        );

        let storage = provider
            .provide(&filter)
            .await
            .expect("negentropy storage result");

        assert_eq!(storage.size().expect("sealed storage size"), 1);
        assert_eq!(
            storage.get_item(0).expect("storage item"),
            Some(Item::with_timestamp_and_id(
                1_776_000_060,
                Id::from_byte_array(*matching.bytes()),
            )),
        );
    }

    /// The NIP-77 infinity timestamp must not be inserted as a real record.
    #[test]
    fn insert_negentropy_record_skips_infinity_timestamp() {
        let valid_id = Id::from_byte_array([1; 32]);
        let invalid_id = Id::from_byte_array([2; 32]);
        let mut storage = NegentropyStorageVector::new();

        insert_negentropy_record(&mut storage, 1_776_000_050, valid_id);
        insert_negentropy_record(&mut storage, u64::MAX, invalid_id);
        storage.seal().expect("sealed storage");

        assert_eq!(storage.size().expect("sealed storage size"), 1);
        assert_eq!(
            storage.get_item(0).expect("storage item"),
            Some(Item::with_timestamp_and_id(1_776_000_050, valid_id)),
        );
    }

    /// The real nostrdb-backed event checker should remove already-present ids
    /// and leave missing ids untouched.
    #[test]
    fn ndb_event_checker_retain_missing_filters_present_ids() {
        let (_tmp, ndb) = test_ndb();
        let present = ingest_text_note(&ndb, 1, "present", 1_776_000_040);
        wait_for_note(&ndb, present);
        let checker = NdbEventChecker { ndb };
        let missing = NoteId::new({
            let mut bytes = *present.bytes();
            bytes[0] ^= 0xFF;
            bytes
        });
        let mut ids = HashSet::from([present, missing]);
        checker.retain_missing(&mut ids);

        assert!(!ids.contains(&present));
        assert!(ids.contains(&missing));
        assert_eq!(ids.len(), 1);
    }
}
