//! A user's keymap overrides, stored in nostrdb as a private note.
//!
//! The overrides are a TOML keymap (see [`parse_over`](super::parse_over)
//! for what one can say) carried as the content of a NIP-78
//! application-data note: kind [`KIND_APP_DATA`], `d` = [`KEYMAP_D`], signed
//! by the account. It is written PNS-wrapped through
//! [`private_note::write`], so it never leaves the device in the
//! clear, and syncs to the account's other devices over its private relays
//! with the rest of its PNS stream. nostrdb unwraps it on ingest, so it reads
//! back like any other note of the account's; a watch-only account can't
//! unwrap it and gets the defaults.

use nostrdb::{Filter, Ndb, Transaction};
use nostrdb_net::Pubkey;

/// NIP-78 application-specific data: parameterized-replaceable, so the newest
/// revision of a `d` is the one that counts.
pub const KIND_APP_DATA: u32 = 30078;

/// The `d` tag of the keymap's [`KIND_APP_DATA`] note.
pub const KEYMAP_D: &str = "notedeck/keymap";

/// The filter for `author`'s app-data notes, the keymap's among them: what
/// a subscription that watches for keymap edits listens on. The `d` tag is
/// checked when reading (see [`load_user_keymap`]).
pub fn user_keymap_filter(author: &Pubkey) -> Filter {
    Filter::new()
        .kinds([KIND_APP_DATA as u64])
        .authors([author.bytes()])
        .build()
}

/// `author`'s keymap slot: their [`KIND_APP_DATA`] note with `d` =
/// [`KEYMAP_D`].
fn keymap_slot(author: &Pubkey) -> private_note::Slot<'_> {
    private_note::Slot {
        author,
        kind: KIND_APP_DATA,
        d: KEYMAP_D,
    }
}

/// The TOML of `author`'s keymap overrides, or `None` when they have none
/// (or the account is watch-only, so nostrdb couldn't unwrap the note). The
/// newest revision wins, ties broken by note id ([`private_note::latest`]).
pub fn load_user_keymap(ndb: &Ndb, author: &Pubkey) -> Option<String> {
    let txn = Transaction::new(ndb).ok()?;
    private_note::latest(ndb, &txn, keymap_slot(author)).map(|revision| revision.content)
}

/// Write `toml` as `author`'s keymap overrides, signed with their `secret`,
/// returning the `created_at` it was stamped with.
///
/// Stamped strictly after both the stored revision and `after`, the stamp of
/// the last write the caller made, so a re-save in the same second, or one
/// made before the last has landed, still supersedes (see
/// [`private_note::write`]). Doesn't check `toml`: that's
/// [`Chrome::save_user_keymap`](crate::Chrome::save_user_keymap)'s job,
/// which merges it first.
pub fn write_user_keymap(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    toml: &str,
    after: u64,
) -> Result<u64, String> {
    private_note::write(ndb, secret, keymap_slot(author), toml, after)
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostrdb::{Config, NoteBuilder, SubscriptionStream};
    use nostrdb_net::FullKeypair;
    use std::time::Duration;

    /// How long a test waits for nostrdb to ingest what it wrote.
    const INGEST_TIMEOUT: Duration = Duration::from_secs(10);

    /// A fresh ndb in `dir` that unwraps `kp`'s PNS notes.
    fn ndb_for(dir: &tempfile::TempDir, kp: &FullKeypair) -> Ndb {
        let ndb = Ndb::new(dir.path().to_str().expect("path"), &Config::new()).expect("ndb");
        assert!(ndb.add_key(&kp.secret_key.secret_bytes()));
        ndb
    }

    /// Wait until `author`'s keymap reads back as `want`. Checks first, so a
    /// write already ingested doesn't wait for a note that won't come.
    async fn wait_keymap(ndb: &Ndb, stream: &mut SubscriptionStream, author: &Pubkey, want: &str) {
        while load_user_keymap(ndb, author).as_deref() != Some(want) {
            stream
                .wait_for_notes(1, INGEST_TIMEOUT)
                .await
                .expect("the keymap note ingested before the deadline");
        }
    }

    #[tokio::test]
    async fn a_saved_keymap_reads_back_and_a_resave_supersedes_it() {
        let dir = tempfile::TempDir::new().expect("tmp dir");
        let kp = FullKeypair::generate();
        let ndb = ndb_for(&dir, &kp);
        let secret = kp.secret_key.secret_bytes();
        // Subscribed before the first write, so no ingest slips past.
        let sub = ndb
            .subscribe(&[user_keymap_filter(&kp.pubkey)])
            .expect("subscribe");
        let mut stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);

        assert_eq!(load_user_keymap(&ndb, &kp.pubkey), None);

        let first = "[[bind]]\nscope = \"s\"\nkeys = \"j\"\ntool = \"a\"\n";
        write_user_keymap(&ndb, &kp.pubkey, &secret, first, 0).expect("write");
        wait_keymap(&ndb, &mut stream, &kp.pubkey, first).await;

        // In the same second: stamped past the first, so it still wins.
        let second = "[[bind]]\nscope = \"s\"\nkeys = \"j\"\ntool = \"b\"\n";
        write_user_keymap(&ndb, &kp.pubkey, &secret, second, 0).expect("write");
        wait_keymap(&ndb, &mut stream, &kp.pubkey, second).await;
    }

    #[tokio::test]
    async fn another_app_data_note_is_not_the_keymap() {
        let dir = tempfile::TempDir::new().expect("tmp dir");
        let kp = FullKeypair::generate();
        let ndb = ndb_for(&dir, &kp);
        let secret = kp.secret_key.secret_bytes();
        let sub = ndb
            .subscribe(&[user_keymap_filter(&kp.pubkey)])
            .expect("subscribe");
        let mut stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);

        // Another app's NIP-78 note, written after the keymap.
        write_user_keymap(&ndb, &kp.pubkey, &secret, "", 0).expect("write");
        let other = NoteBuilder::new()
            .kind(KIND_APP_DATA)
            .content("not a keymap")
            .created_at(notedeck::unix_time_secs() + 60)
            .start_tag()
            .tag_str("d")
            .tag_str("someone-else/settings")
            .sign(&secret)
            .build()
            .expect("note");
        notedeck::write_private_note(&ndb, &secret, &other.json().expect("json")).expect("write");
        stream
            .wait_for_notes(2, INGEST_TIMEOUT)
            .await
            .expect("both ingested");

        assert_eq!(load_user_keymap(&ndb, &kp.pubkey).as_deref(), Some(""));
    }

    #[tokio::test]
    async fn a_resave_before_the_last_lands_still_supersedes_it() {
        let dir = tempfile::TempDir::new().expect("tmp dir");
        let kp = FullKeypair::generate();
        let ndb = ndb_for(&dir, &kp);
        let secret = kp.secret_key.secret_bytes();
        let sub = ndb
            .subscribe(&[user_keymap_filter(&kp.pubkey)])
            .expect("subscribe");
        let mut stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);

        // Back to back, the second written before the first is stored: only
        // the stamp the first returned keeps them apart.
        let mut last = 0;
        for tool in ["a", "b", "c", "d"] {
            let toml = format!("[[bind]]\nscope = \"s\"\nkeys = \"j\"\ntool = \"{tool}\"\n");
            let stamped = write_user_keymap(&ndb, &kp.pubkey, &secret, &toml, last).expect("write");
            assert!(stamped > last);
            last = stamped;
        }
        stream
            .wait_for_notes(4, INGEST_TIMEOUT)
            .await
            .expect("all four ingested");
        let newest = load_user_keymap(&ndb, &kp.pubkey).expect("a keymap");
        assert!(newest.contains("tool = \"d\""), "{newest}");
    }
}
