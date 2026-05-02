//! NostrDB ingest filters for Notedeck.
//!
//! Notedeck primarily relies on `nostrdb` for persistence and queries. `nostrdb` verifies event
//! signatures on ingest by default, but some *local-only* derived events are intentionally unsigned
//! (or otherwise un-verifiable). In those cases we opt into `nostrdb`'s "skip validation" ingest
//! action for a narrowly-scoped set of events.

use std::ffi::c_void;

use nostrdb::Config;

/// Signature prefix used for Notedeck's double-ratchet inner-rumor ingest marker.
///
/// These events are derived from decrypted `nostr-double-ratchet` payloads and are *not* valid
/// signed Nostr events (we don't have the peer's secret key). The signature field is repurposed as
/// a marker so we can skip signature verification at ingest time.
pub const DOUBLE_RATCHET_SIG_PREFIX: [u8; 4] = *b"nddr";

const NDB_INGEST_ACCEPT: u32 = 1;
const NDB_INGEST_SKIP_VALIDATION: u32 = 2;

const NIP17_CHAT_MESSAGE_KIND: u32 = 14;
const IRIS_GROUP_METADATA_KIND: u32 = 40;
const IRIS_CHAT_SETTINGS_KIND: u32 = 10448;

/// Install an ingest filter that allows Notedeck to persist double-ratchet inner rumor events.
///
/// The filter skips signature validation for `nostr-double-ratchet` *inner rumor* events whose
/// signature begins with [`DOUBLE_RATCHET_SIG_PREFIX`]. The marker is only written by Notedeck when
/// it derives local plaintext from a decrypted outer event, and the bypass is intentionally limited
/// to the kinds Notedeck stores as unsigned derived events:
/// - kind 14 (chat messages)
/// - kind 40 (Iris group metadata/control)
/// - kind 10448 (Iris 1:1 chat settings)
///
/// All other events are validated normally.
pub fn install_double_ratchet_ingest_filter(config: &mut Config) {
    unsafe {
        let config_ptr = &mut config.config as *mut _ as *mut c_void;
        ndb_config_set_ingest_filter(
            config_ptr,
            Some(double_ratchet_ingest_filter),
            std::ptr::null_mut(),
        );
    }
}

extern "C" fn double_ratchet_ingest_filter(_ctx: *mut c_void, note: *mut c_void) -> u32 {
    if note.is_null() {
        return NDB_INGEST_ACCEPT;
    }

    unsafe {
        let kind = ndb_note_kind(note);
        if !matches!(
            kind,
            NIP17_CHAT_MESSAGE_KIND | IRIS_GROUP_METADATA_KIND | IRIS_CHAT_SETTINGS_KIND
        ) {
            return NDB_INGEST_ACCEPT;
        }

        let sig_ptr = ndb_note_sig(note);
        if sig_ptr.is_null() {
            return NDB_INGEST_ACCEPT;
        }

        let sig = std::slice::from_raw_parts(sig_ptr as *const u8, 64);
        if sig.starts_with(&DOUBLE_RATCHET_SIG_PREFIX) {
            return NDB_INGEST_SKIP_VALIDATION;
        }
    }

    NDB_INGEST_ACCEPT
}

extern "C" {
    fn ndb_config_set_ingest_filter(
        config: *mut c_void,
        filter: Option<extern "C" fn(*mut c_void, *mut c_void) -> u32>,
        ctx: *mut c_void,
    );

    fn ndb_note_kind(note: *mut c_void) -> u32;
    fn ndb_note_sig(note: *mut c_void) -> *mut u8;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    use nostr::{EventBuilder, Keys, Kind, Tag, Timestamp, UnsignedEvent};
    use nostrdb::{Filter, Ndb, Transaction};

    fn wait_for_note(ndb: &Ndb, id: &[u8; 32], timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let txn = Transaction::new(ndb).expect("txn");
            if ndb.get_note_by_id(&txn, id).is_ok() {
                return true;
            }

            if Instant::now() >= deadline {
                return false;
            }

            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn build_marked_inner_rumor(
        pubkey: nostr::PublicKey,
        kind: u16,
    ) -> (UnsignedEvent, String, [u8; 32]) {
        let recipient = Keys::generate().public_key();
        let recipient_hex = hex::encode(recipient.to_bytes());

        let created_at = Timestamp::from(1_700_000_000u64);
        let tags = vec![
            Tag::parse(&["p".to_string(), recipient_hex]).expect("p tag"),
            Tag::parse(&["ms".to_string(), "1".to_string()]).expect("ms tag"),
        ];

        let mut event = EventBuilder::new(Kind::from(kind), "hello")
            .custom_created_at(created_at)
            .tags(tags)
            .build(pubkey);
        event.ensure_id();

        let id = event.id.expect("id").to_bytes();

        let mut sig = [0u8; 64];
        sig[..DOUBLE_RATCHET_SIG_PREFIX.len()].copy_from_slice(&DOUBLE_RATCHET_SIG_PREFIX);
        let sig_hex = hex::encode(sig);

        (event, sig_hex, id)
    }

    fn encode_event_with_sig(event: &UnsignedEvent, sig_hex: &str) -> String {
        let mut v = serde_json::to_value(event).expect("to_value");
        let obj = v.as_object_mut().expect("event json object");
        obj.insert(
            "sig".to_string(),
            serde_json::Value::String(sig_hex.to_string()),
        );
        serde_json::to_string(&v).expect("to_string")
    }

    #[test]
    fn double_ratchet_marker_sig_is_rejected_without_filter_but_accepted_with_filter() {
        let keys = Keys::generate();
        // Kinds we persist locally from decrypted double-ratchet "inner rumor" events.
        const IRIS_GROUP_METADATA_KIND: u16 = 40;
        const IRIS_CHAT_SETTINGS_KIND: u16 = 10448;
        const TEST_KINDS: [u16; 3] = [14, IRIS_GROUP_METADATA_KIND, IRIS_CHAT_SETTINGS_KIND];

        for kind in TEST_KINDS {
            let (event, sig_hex, id) = build_marked_inner_rumor(keys.public_key(), kind);
            let json = encode_event_with_sig(&event, &sig_hex);

            let tmp = tempfile::tempdir().expect("tempdir");

            // No filter: invalid signature should be rejected by the ingester.
            let config = Config::new()
                .set_mapsize(64 * 1024 * 1024)
                .set_ingester_threads(1);
            let ndb = Ndb::new(tmp.path().to_str().unwrap(), &config).expect("ndb");
            let _ = ndb.process_client_event(&json);
            assert!(
                !wait_for_note(&ndb, &id, Duration::from_millis(200)),
                "kind {kind} note should not be persisted without filter"
            );

            // With filter: the same note should be ingested.
            let tmp2 = tempfile::tempdir().expect("tempdir2");
            let mut config2 = Config::new()
                .set_mapsize(64 * 1024 * 1024)
                .set_ingester_threads(1);
            install_double_ratchet_ingest_filter(&mut config2);
            let ndb2 = Ndb::new(tmp2.path().to_str().unwrap(), &config2).expect("ndb2");
            let _ = ndb2.process_client_event(&json);
            assert!(
                wait_for_note(&ndb2, &id, Duration::from_secs(2)),
                "kind {kind} note should be persisted once the filter is installed"
            );

            // Sanity: query by kind should see the stored event.
            let txn = Transaction::new(&ndb2).expect("txn2");
            let results = ndb2
                .query(&txn, &[Filter::new().kinds([kind.into()]).build()], 10)
                .expect("query");
            assert!(
                results.iter().any(|r| r.note.id() == &id),
                "expected kind {kind} query to include stored note"
            );
        }
    }
}
