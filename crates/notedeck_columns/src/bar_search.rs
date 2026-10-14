//! Columns' answer to the chrome's universal search (the Cmd-K bar, see
//! [`notedeck::search`]): profiles by name, notes by their text, and a
//! hashtag's timeline.
//!
//! - **Profiles** come from nostrdb's profile name index the way the search
//!   view's people list finds them ([`search_profiles`]: contacts first, a
//!   pasted `npub`/`nprofile` as that profile). The index lookup is cheap, so
//!   they're answered at once, on every ask.
//! - **Notes** come from nostrdb's full-text index, which is not cheap. The
//!   query has to stand for [`SETTLE`] before a full-text query runs, so
//!   typing a word doesn't start one per letter, and it runs on the
//!   [`JobPool`], never on the render thread. Until it's back the answer is
//!   [pending](SearchHits::set_pending), and the chrome asks again.
//! - **`#tag`** is offered as that hashtag's timeline, ahead of the rest:
//!   a row titled `#tag` whose subtitle says it opens the timeline.
//!
//! A profile opens through its `nostr:npub…` and a hashtag through its
//! `#tag`, both of which the chrome opens in Columns as a click on them
//! would (see [`notedeck::reference_note_action`]); a note opens through its
//! `nostr:note1…`, which the built-in nostr parser resolves.

use std::time::{Duration, Instant};

use nostrdb::{Filter, Ndb, NoteKey, Transaction};
use nostrdb_net::{NoteId, Pubkey};
use notedeck::fuzzy::fuzzy_match;
use notedeck::name::get_display_name;
use notedeck::{
    tr, ContactState, JobPool, Localization, OpenUri, SearchHit, SearchHits, SearchQuery,
};
use notedeck_ui::contacts_list::search_profiles;
use tokio::sync::oneshot;

/// How long the query must stand before a full-text note query runs for it.
pub(crate) const SETTLE: Duration = Duration::from_millis(150);

/// The most profiles listed, so notes still get rows under the cap.
const MAX_PROFILES: usize = 4;

/// The most notes one full-text query asks nostrdb for.
const MAX_NOTES: u64 = 8;

/// The longest a note's text is drawn as a hit's title, in chars.
const TITLE_CHARS: usize = 80;

/// What a profile hit is marked as in the bar.
const KIND_PROFILE: &str = "Profile";

/// What a note hit is marked as in the bar.
const KIND_NOTE: &str = "Note";

/// What a hashtag hit is marked as in the bar.
const KIND_HASHTAG: &str = "Hashtag";

/// Where the full-text half of the search is for the query it answers.
#[derive(Default)]
enum NoteSearch {
    /// Nothing asked yet, or a query with no notes to look for.
    #[default]
    Idle,
    /// Asked about `text`, waiting for it to stand for [`SETTLE`].
    Settling { text: String, since: Instant },
    /// `text`'s query is running on the job pool.
    Running {
        text: String,
        rx: oneshot::Receiver<Vec<NoteKey>>,
    },
    /// `text`'s query came back with these notes, best first.
    Done { text: String, keys: Vec<NoteKey> },
}

impl NoteSearch {
    /// The text this state answers, if any.
    fn text(&self) -> Option<&str> {
        match self {
            NoteSearch::Idle => None,
            NoteSearch::Settling { text, .. }
            | NoteSearch::Running { text, .. }
            | NoteSearch::Done { text, .. } => Some(text),
        }
    }
}

/// The search state Columns keeps between the bar's asks: only the
/// full-text half, since profiles are looked up afresh each time.
#[derive(Default)]
pub struct BarSearch {
    notes: NoteSearch,
}

/// What [`BarSearch::search`] reads from the app's context.
pub(crate) struct BarSearchCtx<'a> {
    pub ndb: &'a Ndb,
    pub jobs: &'a JobPool,
    pub contacts: &'a ContactState,
    pub i18n: &'a mut Localization,
}

/// What a query asks of each half of the search, once its form is read.
struct Parsed<'q> {
    /// The hashtag to offer, without its `#`.
    hashtag: Option<&'q str>,
    /// What to look profiles up by, if anything.
    profile: Option<&'q str>,
    /// What to look notes up by, if anything.
    notes: Option<&'q str>,
}

impl<'q> Parsed<'q> {
    fn new(text: &'q str) -> Self {
        // A pasted reference: a profile one is looked up as the profile, and
        // a note one is the chrome's own "Open" row.
        if let Some(bech) = text.strip_prefix("nostr:") {
            let is_profile = bech.starts_with("npub1") || bech.starts_with("nprofile1");
            return Self {
                hashtag: None,
                profile: is_profile.then_some(bech),
                notes: None,
            };
        }
        if let Some(tag) = text.strip_prefix('#') {
            let is_word = !tag.is_empty() && !tag.contains(|c: char| c.is_whitespace() || c == '#');
            return Self {
                hashtag: is_word.then_some(tag),
                profile: None,
                notes: is_word.then_some(tag),
            };
        }
        Self {
            hashtag: None,
            profile: Some(text),
            notes: Some(text),
        }
    }
}

impl BarSearch {
    /// Answer `query` into `out` at `now`: the hashtag row, then profiles,
    /// then notes once their full-text query is back, up to the cap. Marks
    /// the answer pending while the notes are still to come.
    #[profiling::function]
    pub(crate) fn search(
        &mut self,
        ctx: BarSearchCtx<'_>,
        query: &SearchQuery<'_>,
        now: Instant,
        out: &mut SearchHits,
    ) {
        let parsed = Parsed::new(query.text);
        let Ok(txn) = Transaction::new(ctx.ndb) else {
            return;
        };

        if let Some(tag) = parsed.hashtag {
            // Titled by the hashtag itself, so the query matches it whole;
            // what choosing it does goes in the subtitle.
            let subtitle = tr!(
                ctx.i18n,
                "Open timeline",
                "Universal search: subtitle of the row opening a hashtag's timeline"
            );
            let reference = format!("#{tag}");
            out.push(
                SearchHit::new(reference.clone(), OpenUri::new(reference), 0)
                    .kind(KIND_HASHTAG)
                    .subtitled(subtitle),
            );
        }

        if let Some(text) = parsed.profile {
            push_profiles(ctx.ndb, &txn, ctx.contacts, text, out);
        }

        let Some(text) = parsed.notes else {
            self.notes = NoteSearch::Idle;
            return;
        };
        if self.notes.text() != Some(text) {
            self.notes = NoteSearch::Settling {
                text: text.to_owned(),
                since: now,
            };
        }
        if self.advance(ctx.ndb, ctx.jobs, now) {
            out.set_pending();
            return;
        }
        if let NoteSearch::Done { keys, .. } = &self.notes {
            push_notes(ctx.ndb, &txn, keys, out);
        }
    }

    /// Move the full-text half on as far as `now` allows: start the query
    /// once the text has settled, take its notes once they're back. Returns
    /// whether the notes are still to come.
    fn advance(&mut self, ndb: &Ndb, jobs: &JobPool, now: Instant) -> bool {
        match std::mem::take(&mut self.notes) {
            NoteSearch::Settling { text, since } if now.duration_since(since) < SETTLE => {
                self.notes = NoteSearch::Settling { text, since };
                true
            }
            NoteSearch::Settling { text, .. } => {
                let rx = jobs.schedule_receivable(full_text_job(ndb.clone(), text.clone()));
                self.notes = NoteSearch::Running { text, rx };
                true
            }
            NoteSearch::Running { text, mut rx } => match rx.try_recv() {
                Ok(keys) => {
                    self.notes = NoteSearch::Done { text, keys };
                    false
                }
                Err(oneshot::error::TryRecvError::Empty) => {
                    self.notes = NoteSearch::Running { text, rx };
                    true
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.notes = NoteSearch::Done {
                        text,
                        keys: Vec::new(),
                    };
                    false
                }
            },
            done => {
                self.notes = done;
                false
            }
        }
    }
}

/// The full-text query for `text`, to run on the job pool: kind-1 notes
/// nostrdb's text index matches, best first.
fn full_text_job(ndb: Ndb, text: String) -> impl FnOnce() -> Vec<NoteKey> + Send + 'static {
    move || {
        let Ok(txn) = Transaction::new(&ndb) else {
            return Vec::new();
        };
        let filter = Filter::new()
            .search(&text)
            .kinds([1])
            .limit(MAX_NOTES)
            .build();
        let keys = match ndb.query(&txn, &[filter], MAX_NOTES as i32) {
            Ok(results) => results.into_iter().map(|r| r.note_key).collect(),
            Err(err) => {
                tracing::error!("bar search: full-text query for {text:?} failed: {err}");
                Vec::new()
            }
        };
        keys
    }
}

/// Push the profiles `text` finds, contacts first, up to [`MAX_PROFILES`].
fn push_profiles(
    ndb: &Ndb,
    txn: &Transaction,
    contacts: &ContactState,
    text: &str,
    out: &mut SearchHits,
) {
    let mut positions = Vec::new();
    for found in search_profiles(ndb, txn, text, contacts, MAX_PROFILES) {
        let pubkey = Pubkey::new(found.pk);
        let Some(npub) = pubkey.npub() else {
            continue;
        };
        let record = ndb.get_profile_by_pubkey(txn, &found.pk).ok();
        let name = get_display_name(record.as_ref());
        let title = match name.display_name.or(name.username) {
            Some(title) => title.to_owned(),
            None => abbreviated(&npub),
        };
        let score = fuzzy_match(text, &title, &mut positions)
            .or_else(|| fuzzy_match(text, name.username?, &mut positions))
            .unwrap_or(0);
        let mut hit =
            SearchHit::new(title, OpenUri::new(format!("nostr:{npub}")), score).kind(KIND_PROFILE);
        if let Some(subtitle) = profile_subtitle(name.username, name.display_name, name.nip05) {
            hit = hit.subtitled(subtitle);
        }
        if !out.push(hit) {
            return;
        }
    }
}

/// A profile's handle and NIP-05, where they add to its title: `@name` when
/// a display name is the title, then the NIP-05.
fn profile_subtitle(
    username: Option<&str>,
    display_name: Option<&str>,
    nip05: Option<&str>,
) -> Option<String> {
    let handle = username.filter(|_| display_name.is_some());
    match (handle, nip05) {
        (Some(handle), Some(nip05)) => Some(format!("@{handle} · {nip05}")),
        (Some(handle), None) => Some(format!("@{handle}")),
        (None, Some(nip05)) => Some(nip05.to_owned()),
        (None, None) => None,
    }
}

/// Push the notes at `keys` until the cap, each titled by its text and
/// subtitled by its author.
fn push_notes(ndb: &Ndb, txn: &Transaction, keys: &[NoteKey], out: &mut SearchHits) {
    for key in keys {
        let Ok(note) = ndb.get_note_by_key(txn, *key) else {
            continue;
        };
        let Some(bech) = NoteId::new(*note.id()).to_bech() else {
            continue;
        };
        let title = note_title(note.content());
        let mut hit =
            SearchHit::new(title, OpenUri::new(format!("nostr:{bech}")), 0).kind(KIND_NOTE);
        let record = ndb.get_profile_by_pubkey(txn, note.pubkey()).ok();
        let author = get_display_name(record.as_ref());
        if let Some(author) = author.display_name.or(author.username) {
            hit = hit.subtitled(author);
        }
        if !out.push(hit) {
            return;
        }
    }
}

/// A note's text as one line of a title: its words with every run of
/// whitespace as one space, cut to [`TITLE_CHARS`] with an ellipsis.
fn note_title(content: &str) -> String {
    let mut title = String::new();
    for (count, word) in content.split_whitespace().enumerate() {
        if count > 0 {
            title.push(' ');
        }
        title.push_str(word);
        if title.chars().count() > TITLE_CHARS {
            break;
        }
    }
    if title.chars().count() <= TITLE_CHARS {
        return title;
    }
    let mut cut: String = title.chars().take(TITLE_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// `npub` shortened to its first and last few characters, for a profile
/// with no name.
fn abbreviated(npub: &str) -> String {
    let head = npub.get(..12).unwrap_or(npub);
    let tail = npub.get(npub.len().saturating_sub(6)..).unwrap_or("");
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostrdb::{IngestMetadata, NoteBuilder};
    use nostrdb_net::FullKeypair;
    use notedeck_testing::fixtures::TempNdb;

    /// Sign a note of `kind` with `content` by `kp` at `created_at`.
    fn signed(
        kp: &FullKeypair,
        kind: u32,
        content: &str,
        created_at: u64,
    ) -> nostrdb::Note<'static> {
        NoteBuilder::new()
            .kind(kind)
            .content(content)
            .created_at(created_at)
            .sign(&kp.secret_key.secret_bytes())
            .build()
            .expect("note")
    }

    /// Ingest `notes` into `ndb` and wait until every one is committed.
    async fn ingest(ndb: &Ndb, notes: &[nostrdb::Note<'_>]) {
        let filter = Filter::new().kinds([0, 1]).build();
        let sub = ndb.subscribe(&[filter]).expect("subscribe");
        for note in notes {
            let frame = nostrdb_net::ClientMessage::event(note)
                .expect("event")
                .to_json()
                .expect("json");
            ndb.process_event_with(&frame, IngestMetadata::new().client(true))
                .expect("ingest");
        }
        ndb.wait_for_notes(sub, notes.len() as u32)
            .await
            .expect("committed");
    }

    /// Ask until the answer is no longer pending, the clock moving past
    /// [`SETTLE`] after the first ask, for at most a few seconds.
    fn answer(search: &mut BarSearch, ndb: &Ndb, jobs: &JobPool, text: &str) -> Vec<SearchHit> {
        let mut i18n = Localization::default();
        let contacts = ContactState::Unreceived;
        let query = SearchQuery { text };
        let start = Instant::now();
        let deadline = start + Duration::from_secs(5);
        let mut now = start;
        loop {
            let mut out = SearchHits::new(notedeck::search::HITS_PER_PROVIDER);
            let ctx = BarSearchCtx {
                ndb,
                jobs,
                contacts: &contacts,
                i18n: &mut i18n,
            };
            search.search(ctx, &query, now, &mut out);
            if !out.is_pending() {
                return out.hits().to_vec();
            }
            assert!(Instant::now() < deadline, "{text:?} stayed pending");
            std::thread::yield_now();
            now = Instant::now().max(start + SETTLE);
        }
    }

    /// A name finds its profile, opening through its `npub`; a word of a
    /// note's text finds the note, titled by that text, subtitled by its
    /// author, opening through its `note1`; neither finds the other.
    #[tokio::test]
    async fn a_name_finds_the_profile_and_a_word_finds_the_note() {
        let tmp = TempNdb::new();
        let jobs = JobPool::new(1);
        let kp = FullKeypair::generate();
        let profile = signed(
            &kp,
            0,
            r#"{"name":"quokka","display_name":"Quentin Quokka","nip05":"q@example.com"}"#,
            1_000,
        );
        let note = signed(&kp, 1, "the  zebra\ncrossing is open", 1_001);
        let note_id = NoteId::new(*note.id());
        ingest(&tmp.ndb, &[profile, note]).await;
        let mut search = BarSearch::default();

        let hits = answer(&mut search, &tmp.ndb, &jobs, "quokka");
        let npub = kp.pubkey.npub().unwrap();
        let found = hits
            .iter()
            .find(|hit| hit.kind.as_deref() == Some(KIND_PROFILE));
        let found = found.unwrap_or_else(|| panic!("no profile in {hits:?}"));
        assert_eq!(found.title, "Quentin Quokka");
        assert_eq!(found.subtitle.as_deref(), Some("@quokka · q@example.com"));
        assert_eq!(found.uri.reference, format!("nostr:{npub}"));
        assert!(hits
            .iter()
            .all(|hit| hit.kind.as_deref() != Some(KIND_NOTE)));

        let hits = answer(&mut search, &tmp.ndb, &jobs, "zebra");
        let bech = note_id.to_bech().unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].title, "the zebra crossing is open");
        assert_eq!(hits[0].kind.as_deref(), Some(KIND_NOTE));
        assert_eq!(hits[0].subtitle.as_deref(), Some("Quentin Quokka"));
        assert_eq!(hits[0].uri.reference, format!("nostr:{bech}"));
    }

    /// Notes wait for the query to settle: a first ask is pending with the
    /// profiles already in it, and starts no full-text query until
    /// [`SETTLE`] has passed; a new query starts the wait again.
    #[test]
    fn notes_wait_for_the_query_to_settle() {
        let tmp = TempNdb::new();
        let jobs = JobPool::new(1);
        let mut i18n = Localization::default();
        let contacts = ContactState::Unreceived;
        let mut search = BarSearch::default();
        let t0 = Instant::now();
        let mut ask = |search: &mut BarSearch, text: &str, now: Instant| {
            let mut out = SearchHits::new(notedeck::search::HITS_PER_PROVIDER);
            let ctx = BarSearchCtx {
                ndb: &tmp.ndb,
                jobs: &jobs,
                contacts: &contacts,
                i18n: &mut i18n,
            };
            search.search(ctx, &SearchQuery { text }, now, &mut out);
            out.is_pending()
        };

        assert!(ask(&mut search, "zeb", t0));
        assert!(ask(&mut search, "zeb", t0 + SETTLE / 2));
        assert!(matches!(search.notes, NoteSearch::Settling { .. }));
        assert!(ask(&mut search, "zebra", t0 + SETTLE));
        assert!(
            matches!(&search.notes, NoteSearch::Settling { since, .. } if *since == t0 + SETTLE),
            "a new query waits afresh"
        );
        assert!(ask(&mut search, "zebra", t0 + SETTLE * 2));
        assert!(matches!(search.notes, NoteSearch::Running { .. }));
    }

    /// `#tag` is offered as its timeline first, opening through `#tag`, and
    /// a pasted profile reference looks the profile up without a note
    /// query; a pasted note reference is left to the chrome.
    #[tokio::test]
    async fn hashtags_and_pasted_references() {
        let tmp = TempNdb::new();
        let jobs = JobPool::new(1);
        let kp = FullKeypair::generate();
        let profile = signed(&kp, 0, r#"{"name":"wombat"}"#, 1_000);
        ingest(&tmp.ndb, &[profile]).await;
        let mut search = BarSearch::default();

        let hits = answer(&mut search, &tmp.ndb, &jobs, "#zebra");
        assert_eq!(hits[0].title, "#zebra");
        assert_eq!(hits[0].subtitle.as_deref(), Some("Open timeline"));
        assert_eq!(hits[0].uri.reference, "#zebra");
        assert_eq!(hits[0].kind.as_deref(), Some(KIND_HASHTAG));

        let npub = kp.pubkey.npub().unwrap();
        let hits = answer(&mut search, &tmp.ndb, &jobs, &format!("nostr:{npub}"));
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].title, "wombat");
        assert!(matches!(search.notes, NoteSearch::Idle));

        let note = format!("nostr:{}", NoteId::new([1; 32]).to_bech().unwrap());
        assert!(answer(&mut search, &tmp.ndb, &jobs, &note).is_empty());
    }

    /// A note's text is one line, cut with an ellipsis past the limit.
    #[test]
    fn note_titles_are_one_line_and_cut() {
        assert_eq!(note_title("  a\n\nb\tc "), "a b c");
        let long = "word ".repeat(40);
        let title = note_title(&long);
        assert_eq!(title.chars().count(), TITLE_CHARS);
        assert!(title.ends_with('…'));
    }
}
