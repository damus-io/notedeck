//! A user's keymap overrides, stored in nostrdb as a private note.
//!
//! The overrides are a TOML keymap (see [`parse_over`](super::parse_over)
//! for what one can say) carried as the content of a NIP-78
//! application-data note: kind [`KIND_APP_DATA`], `d` = [`KEYMAP_D`], signed
//! by the account. It is written PNS-wrapped through
//! [`notedeck::write_private_note`], so it never leaves the device in the
//! clear, and syncs to the account's other devices over its private relays
//! with the rest of its PNS stream. nostrdb unwraps it on ingest, so it reads
//! back like any other note of the account's; a watch-only account can't
//! unwrap it and gets the defaults.

use nostrdb::{Filter, Ndb, Note, NoteBuildOptions, NoteBuilder, Transaction};
use nostrdb_net::Pubkey;

/// NIP-78 application-specific data: parameterized-replaceable, so the newest
/// revision of a `d` is the one that counts.
pub const KIND_APP_DATA: u32 = 30078;

/// The `d` tag of the keymap's [`KIND_APP_DATA`] note.
pub const KEYMAP_D: &str = "notedeck/keymap";

/// The filter for `author`'s app-data notes, the keymap's among them.
///
/// Kind and author only: the `d` tag is checked in Rust (see
/// [`is_keymap_note`]), because nostrdb answers an authors + kinds + tags
/// filter by scanning every note of the author's kinds anyway, slower than
/// filtering the handful of app-data notes ourselves.
pub fn user_keymap_filter(author: &Pubkey) -> Filter {
    Filter::new()
        .kinds([KIND_APP_DATA as u64])
        .authors([author.bytes()])
        .build()
}

/// Whether `note` is a revision of the keymap note: its `d` is [`KEYMAP_D`].
fn is_keymap_note(note: &Note<'_>) -> bool {
    note.tags()
        .iter()
        .find(|tag| tag.count() >= 2 && tag.get_str(0) == Some("d"))
        .is_some_and(|tag| tag.get_str(1) == Some(KEYMAP_D))
}

/// The newest revision of a keymap note.
struct Revision {
    /// When it was written.
    created_at: u64,
    /// Its TOML.
    content: String,
}

/// `author`'s newest keymap revision, if any. Newest by `created_at`, ties
/// broken by note id, so two revisions stamped in the same second resolve the
/// same way on every device and every run (nostrdb hands them back in no
/// particular order).
fn current(ndb: &Ndb, author: &Pubkey) -> Option<Revision> {
    let txn = Transaction::new(ndb).ok()?;
    let newest = ndb
        .fold(
            &txn,
            &[user_keymap_filter(author)],
            None::<Note<'_>>,
            |best, note| {
                if !is_keymap_note(&note) {
                    return best;
                }
                match best {
                    Some(b) if (b.created_at(), b.id()) >= (note.created_at(), note.id()) => {
                        Some(b)
                    }
                    _ => Some(note),
                }
            },
        )
        .ok()??;
    Some(Revision {
        created_at: newest.created_at(),
        content: newest.content().to_owned(),
    })
}

/// The TOML of `author`'s keymap overrides, or `None` when they have none
/// (or the account is watch-only, so nostrdb couldn't unwrap the note).
pub fn load_user_keymap(ndb: &Ndb, author: &Pubkey) -> Option<String> {
    current(ndb, author).map(|revision| revision.content)
}

/// Write `toml` as `author`'s keymap overrides, signed with their `secret`,
/// returning the `created_at` it was stamped with.
///
/// Stamped strictly after both the stored revision and `after`, so a re-save
/// in the same second still supersedes. `after` is the stamp of the last
/// write the caller made: nostrdb ingests asynchronously, so a write made
/// before the last one has landed can't see it stored, and without `after`
/// would tie it in the same second and lose to it on note id half the time.
/// Doesn't check `toml`: that's
/// [`Chrome::save_user_keymap`](crate::Chrome::save_user_keymap)'s job,
/// which merges it first.
pub fn write_user_keymap(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    toml: &str,
    after: u64,
) -> Result<u64, String> {
    let stored = current(ndb, author).map_or(0, |revision| revision.created_at);
    let created_at = notedeck::unix_time_secs().max(stored.max(after) + 1);
    let note = NoteBuilder::new()
        .kind(KIND_APP_DATA)
        .content(toml)
        .options(NoteBuildOptions::default())
        .created_at(created_at)
        .start_tag()
        .tag_str("d")
        .tag_str(KEYMAP_D)
        .sign(secret)
        .build()
        .ok_or("couldn't build the keymap note")?;
    let json = note.json().map_err(|err| err.to_string())?;
    notedeck::write_private_note(ndb, secret, &json).map_err(|err| err.to_string())?;
    Ok(created_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostrdb::{Config, SubscriptionStream};
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
