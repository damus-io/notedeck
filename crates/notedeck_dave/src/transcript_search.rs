//! Dave's scoped Cmd-K search: the text of the account's agent transcripts.
//!
//! Dave [offers](SearchHits::offer) it beside its session hits on every query
//! ("Search transcripts for “q”"); choosing the offer steps the bar into the
//! [`SCOPE`] level, which asks Dave alone (see [`notedeck::search`]). That
//! answer is too slow for every keystroke, so it runs only on purpose, on a
//! worker.
//!
//! ## The scan
//!
//! One [`try_fold`](nostrdb::Ndb::try_fold) over every kind-1988 conversation
//! note, newest first ([`scan`]). A note counts only when it is the account's
//! and its `role` is [`user` or `assistant`](SEARCHED_ROLES): for those the
//! content is the message body, and tool output (90% of the bytes) is skipped
//! the way `nd agentium grep` folds it by default. The query is a smart-case
//! regex, as `nd agentium grep`'s is, or the literal text when it isn't one.
//!
//! Loading each session's transcript, as `grep::search` does, takes 5.5 s on a
//! real db (4,191 sessions, 705k notes), 98% of it loading rather than
//! matching, and it can't stream. This fold takes ~0.5 s warm, ~15 s cold, and
//! finds the newest sessions' matches in its first milliseconds, so the bar
//! fills on its first refine. The measurements are on
//! `headway:notedeck/cement-grass-nice`.
//!
//! ## The worker
//!
//! A [`TranscriptSearch`] owns one `std::thread` per query: not the shared
//! two-thread `JobPool`, which a 15 s cold scan would starve of image work, and
//! not tokio, since the scan blocks. The thread reads its own clone of the
//! `Ndb` under its own transaction and streams each [`Found`] back over a
//! channel. Each time the bar asks, Dave drains what has come and answers
//! [pending](SearchHits::set_pending) until the thread is done.
//!
//! A query the bar no longer asks about is dropped, which cancels its scan
//! (the fold checks a flag per note): a different query replaces it, an
//! unscoped query (the user stepped out) drops it, and Dave's `update` drops
//! one the bar hasn't asked about for [`IDLE`] (the bar closed). A dropped
//! search's matches never reach an answer, since its channel goes with it.
//!
//! ## Hits
//!
//! One row per session, in the order the scan found them, so the session with
//! the newest match lists first: its display title (what a ref title names,
//! as in [`search`](crate::search), looked up for the listed sessions, deleted
//! ones too), its newest matching line trimmed around
//! the match, and how many messages matched. It opens `agentium:<word-id>`.
//! Deleted sessions are listed, marked [`KIND_DELETED`]: opening one revives
//! it, as a click on its inline chip does. A session with no state event
//! folded can't be opened and isn't listed.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agentium_core::session_events::AI_CONVERSATION_KIND;
use agentium_core::session_fold::SessionReducer;
use agentium_core::wordid;
use cli_term::{compile_pattern, CaseMode};
use nostrdb::{Filter, Ndb, Note, Transaction};
use nostrdb_net::{NoteId, Pubkey};
use notedeck::search::SCOPED_HITS;
use notedeck::{
    tr, tr_plural, AppContext, Localization, OpenUri, ReferenceResolveCtx, SearchHit, SearchHits,
    SearchQuery, SearchScope,
};
use regex::Regex;

use crate::card_titles::CardTitles;
use crate::search::{ref_title, refresh_titles, stale_refs_of};

/// The [key](SearchScope::key) Dave offers its transcript search under.
pub(crate) const SCOPE: &str = "transcripts";

/// What a hit in a live session is marked as in the bar.
const KIND_TRANSCRIPT: &str = "Transcript";

/// What a hit in a deleted session is marked as, so it reads as one before it
/// is opened (which revives it).
const KIND_DELETED: &str = "Deleted transcript";

/// The roles whose notes are searched: the ones whose content is the
/// message a person or the agent wrote.
const SEARCHED_ROLES: [&str; 2] = ["user", "assistant"];

/// How long a search the bar stops asking about is kept before Dave's
/// `update` drops it (see the module docs).
pub(crate) const IDLE: Duration = Duration::from_secs(1);

/// How many characters of a matching line are kept before the match.
const CONTEXT_BEFORE: usize = 40;

/// How many characters of a matching line a hit's subtitle keeps.
const EXCERPT_CHARS: usize = 140;

/// The offer Dave lists beside its session hits for `text`.
pub(crate) fn offer(i18n: &mut Localization, text: &str) -> SearchScope {
    SearchScope {
        key: SCOPE.to_owned(),
        label: tr!(
            i18n,
            "Transcripts",
            "Cmd-K bar: the prompt chip of Dave's transcript search, before the query"
        ),
        title: tr!(
            i18n,
            "Search transcripts for “{query}”",
            "Cmd-K bar: the row offering to search agent session transcripts for what was typed",
            query = text
        ),
    }
}

/// The newest match in one session, and how many of its messages matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionMatch {
    /// The session's `claude_session_id` (the note's `d` tag).
    pub session_id: String,
    /// Its newest matching line, trimmed around the match (see [`excerpt`]).
    pub line: String,
    /// The note that line is from.
    pub note_id: NoteId,
    /// How many of its messages matched so far.
    pub count: usize,
}

/// One match the scan reports.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Found {
    /// The newest match in a session not matched before; the next session
    /// found is numbered after it.
    First(SessionMatch),
    /// Another match in the session found `n`th (from 0).
    Again(usize),
}

/// The pattern `query` searches for: a smart-case regex, as `nd agentium grep`
/// compiles it, or the literal text when it isn't a valid one (`foo(`).
fn pattern(query: &str) -> Regex {
    compile_pattern(query, CaseMode::Smart)
        .or_else(|_| compile_pattern(&regex::escape(query), CaseMode::Smart))
        .unwrap_or_else(|_| Regex::new("$^").expect("an empty pattern compiles"))
}

/// Scan every kind-1988 note `author` wrote, newest first, for `pattern` (see
/// the module docs), handing each match to `found`. Stops early once `cancel`
/// is set; returns whether it ran to the end.
#[profiling::function]
pub(crate) fn scan(
    ndb: &Ndb,
    txn: &Transaction,
    author: &Pubkey,
    pattern: &Regex,
    cancel: &AtomicBool,
    mut found: impl FnMut(Found),
) -> bool {
    let filter = Filter::new().kinds([AI_CONVERSATION_KIND as u64]).build();
    // Each session's number, by its id, in the order first found.
    let mut seen: HashMap<String, usize> = HashMap::new();
    let ran = ndb.try_fold(txn, &[filter], (), |(), note| {
        if cancel.load(Ordering::Relaxed) {
            return ControlFlow::Break(());
        }
        if note.pubkey() != author.bytes() {
            return ControlFlow::Continue(());
        }
        let Some(session_id) = searched_session(&note) else {
            return ControlFlow::Continue(());
        };
        let content = note.content();
        let Some(at) = pattern.find(content) else {
            return ControlFlow::Continue(());
        };
        if let Some(&n) = seen.get(session_id) {
            found(Found::Again(n));
            return ControlFlow::Continue(());
        }
        seen.insert(session_id.to_owned(), seen.len());
        found(Found::First(SessionMatch {
            session_id: session_id.to_owned(),
            line: excerpt(content, at.range()),
            note_id: NoteId::new(*note.id()),
            count: 1,
        }));
        ControlFlow::Continue(())
    });
    ran.is_ok() && !cancel.load(Ordering::Relaxed)
}

/// The session `note` belongs to, when its role is one that is searched.
fn searched_session<'a>(note: &Note<'a>) -> Option<&'a str> {
    let mut session = None;
    let mut role = None;
    for tag in note.tags() {
        match tag.get_str(0) {
            Some("d") => session = tag.get_str(1),
            Some("role") => role = tag.get_str(1),
            _ => {}
        }
    }
    role.filter(|role| SEARCHED_ROLES.contains(role))?;
    session
}

/// The line of `content` holding the match at `at`, cut to [`EXCERPT_CHARS`]
/// with [`CONTEXT_BEFORE`] of them before the match, an ellipsis marking each
/// side that was cut, and its whitespace trimmed.
fn excerpt(content: &str, at: std::ops::Range<usize>) -> String {
    let line_start = content[..at.start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = content[at.end..]
        .find('\n')
        .map_or(content.len(), |i| at.end + i);
    let start = content[line_start..at.start]
        .char_indices()
        .rev()
        .nth(CONTEXT_BEFORE - 1)
        .map_or(line_start, |(i, _)| line_start + i);
    let end = content[start..line_end]
        .char_indices()
        .nth(EXCERPT_CHARS)
        .map_or(line_end, |(i, _)| start + i);
    let mut line = String::new();
    if start > line_start {
        line.push('…');
    }
    line.push_str(content[start..end].trim());
    if end < line_end {
        line.push('…');
    }
    line
}

/// One transcript search on its worker (see the module docs): the query it
/// scans for and what has come back so far. Dropping it cancels the scan.
pub(crate) struct TranscriptSearch {
    /// The query text, as the bar asked it.
    query: String,
    /// Whose transcripts it scans.
    author: Pubkey,
    /// Set to stop the scan.
    cancel: Arc<AtomicBool>,
    /// Matches from the worker; disconnected once it is done.
    rx: Receiver<Found>,
    /// One per session matched, in the order found.
    sessions: Vec<SessionMatch>,
    /// Whether the worker is done and every match has been drained.
    done: bool,
    /// When the bar last asked for this query.
    asked_at: Instant,
}

impl TranscriptSearch {
    /// Start scanning `author`'s transcripts in `ndb` for `query`, on a thread
    /// of its own.
    pub(crate) fn start(ndb: &Ndb, author: Pubkey, query: &str, now: Instant) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let pattern = pattern(query);
        let worker = {
            let ndb = ndb.clone();
            let cancel = cancel.clone();
            move || {
                let Ok(txn) = Transaction::new(&ndb) else {
                    tracing::warn!("transcript search: no read transaction");
                    return;
                };
                scan(&ndb, &txn, &author, &pattern, &cancel, |found| {
                    // A send fails only once the search is dropped, which
                    // also sets `cancel`, so the fold stops at its next note.
                    let _ = tx.send(found);
                });
            }
        };
        let spawned = std::thread::Builder::new()
            .name("dave-transcript-search".to_owned())
            .spawn(worker);
        if let Err(err) = spawned {
            // The closure, and its sender, are dropped: the search reads done.
            tracing::warn!("transcript search: couldn't start its worker: {err}");
        }
        Self {
            query: query.to_owned(),
            author,
            cancel,
            rx,
            sessions: Vec::new(),
            done: false,
            asked_at: now,
        }
    }

    /// Whether this is the search for `query` over `author`'s transcripts.
    fn answers(&self, query: &str, author: &Pubkey) -> bool {
        self.query == query && self.author == *author
    }

    /// Take in what the worker has found since the last poll, and note that
    /// the bar asked at `now`.
    pub(crate) fn poll(&mut self, now: Instant) {
        self.asked_at = now;
        while !self.done {
            match self.rx.try_recv() {
                Ok(found) => self.take(found),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => self.done = true,
            }
        }
    }

    /// Fold one of the worker's matches in.
    fn take(&mut self, found: Found) {
        match found {
            Found::First(session) => self.sessions.push(session),
            Found::Again(n) => {
                if let Some(session) = self.sessions.get_mut(n) {
                    session.count += 1;
                }
            }
        }
    }

    /// Whether the worker is done and all it found has been taken in.
    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    /// One per session matched, the newest match first.
    pub(crate) fn sessions(&self) -> &[SessionMatch] {
        &self.sessions
    }

    /// Whether the bar hasn't asked about it for [`IDLE`] by `now`.
    pub(crate) fn is_idle(&self, now: Instant) -> bool {
        now.duration_since(self.asked_at) >= IDLE
    }

    /// Wait for the worker to finish, taking in everything it found.
    #[cfg(test)]
    fn finish(&mut self) {
        while let Ok(found) = self.rx.recv() {
            self.take(found);
        }
        self.done = true;
    }
}

impl Drop for TranscriptSearch {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The search in `slot` for `query` over `author`'s transcripts, started in
/// `ndb` (replacing, and so cancelling, the one there) unless it is already
/// the one there, then polled at `now`.
pub(crate) fn asked<'a>(
    slot: &'a mut Option<TranscriptSearch>,
    ndb: &Ndb,
    author: Pubkey,
    query: &str,
    now: Instant,
) -> &'a mut TranscriptSearch {
    if !slot.as_ref().is_some_and(|s| s.answers(query, &author)) {
        *slot = None;
    }
    let search = slot.get_or_insert_with(|| TranscriptSearch::start(ndb, author, query, now));
    search.poll(now);
    search
}

/// Answer the bar from `sessions`, the matches so far (newest first), into
/// `out` up to its cap: one hit per session `fold` holds, deleted ones
/// included and marked (see the module docs). `titles` holds the titles of
/// what sessions' titles reference.
#[profiling::function]
pub(crate) fn answer(
    sessions: &[SessionMatch],
    fold: &SessionReducer,
    titles: &CardTitles,
    i18n: &mut Localization,
    out: &mut SearchHits,
) {
    for found in sessions {
        if out.is_full() {
            break;
        }
        let Some(view) = fold.get_including_deleted(&found.session_id) else {
            continue;
        };
        let state = &view.state;
        let title = state.display_title();
        let title = ref_title(titles, title).unwrap_or(title);
        let kind = if state.is_deleted() {
            KIND_DELETED
        } else {
            KIND_TRANSCRIPT
        };
        let subtitle = if found.count > 1 {
            let more = tr_plural!(
                i18n,
                "{count} message",
                "{count} messages",
                "Cmd-K bar: how many of a session's messages matched a transcript search, after its newest matching line",
                found.count as i64,
            );
            format!("{} · {more}", found.line)
        } else {
            found.line.clone()
        };
        // Opened at the matching message rather than the session's top.
        let uri =
            OpenUri::new(wordid::session_ref(&state.claude_session_id)).landing_at(found.note_id);
        // Scored 0: the bar lists a session whose title also matches first,
        // then the rest in the order given, the newest match first.
        out.push(SearchHit::new(title, uri, 0).kind(kind).subtitled(subtitle));
    }
}

impl crate::Dave {
    /// Answer the bar's [`SCOPE`] level for `query`: start or keep its scan,
    /// list what it has found so far, and stay pending until it is done.
    pub(crate) fn search_transcripts(
        &mut self,
        ctx: &mut AppContext<'_>,
        query: &SearchQuery<'_>,
        out: &mut SearchHits,
    ) {
        if query.scope != Some(SCOPE) {
            return;
        }
        let author = *ctx.accounts.selected_account_pubkey();
        let Ok(txn) = Transaction::new(ctx.ndb) else {
            return;
        };
        let search = asked(
            &mut self.transcript_search,
            ctx.ndb,
            author,
            query.text,
            Instant::now(),
        );
        if !search.is_done() {
            out.set_pending();
        }
        self.refresh_hit_titles(ctx, &txn, author);
        let Some(search) = self.transcript_search.as_ref() else {
            return;
        };
        let titles = &self.session_manager.card_titles;
        let i18n = &mut *ctx.i18n;
        let answered = self
            .session_cache
            .borrow_mut()
            .with_fold(ctx.ndb, &txn, &author, |fold| {
                answer(search.sessions(), fold, titles, i18n, out)
            });
        if answered.is_none() {
            // The session fold is still seeding: ask again.
            out.set_pending();
        }
    }

    /// Bring the kept titles of what the listed sessions' titles and issues
    /// reference up to date, as the title search does for live sessions
    /// ([`refresh_search_titles`](Self::refresh_search_titles)), here for the
    /// sessions the transcript search lists, deleted ones included. Skipped
    /// without a window to read the time from.
    fn refresh_hit_titles(&mut self, ctx: &AppContext<'_>, txn: &Transaction, author: Pubkey) {
        let Some(now) = ctx.egui.map(|egui| egui.input(|i| i.time)) else {
            return;
        };
        let Some(search) = self.transcript_search.as_ref() else {
            return;
        };
        let titles = &mut self.session_manager.card_titles;
        let resolve = ReferenceResolveCtx {
            ndb: ctx.ndb,
            txn,
            selected_account: Some(author),
        };
        // Collected under the fold's borrow, resolved after it drops, since
        // resolving an `agentium:` ref reads that same fold.
        let stale = self
            .session_cache
            .borrow_mut()
            .with_fold(ctx.ndb, txn, &author, |fold| {
                let listed = search
                    .sessions()
                    .iter()
                    .filter_map(|found| fold.get_including_deleted(&found.session_id))
                    .take(SCOPED_HITS)
                    .map(|view| &view.state);
                stale_refs_of(listed, ctx.registries, &resolve, titles, now)
            })
            .unwrap_or_default();
        if stale.is_empty() {
            return;
        }
        if refresh_titles(&stale, ctx.registries, &resolve, titles, now) {
            self.session_manager.lineage_changed();
        }
    }

    /// Drop the transcript search the bar has stopped asking about (see the
    /// module docs). Called every frame from `update`.
    pub(crate) fn drop_idle_transcript_search(&mut self) {
        if self
            .transcript_search
            .as_ref()
            .is_some_and(|s| s.is_idle(Instant::now()))
        {
            self.transcript_search = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentium_core::session_events::AI_SESSION_STATE_KIND;
    use agentium_core::session_fold::fold_sessions;
    use agentium_core::session_loader::DELETED_STATUS;
    use nostrdb::{NoteBuilder, SubscriptionStream};
    use nostrdb_net::FullKeypair;
    use notedeck_testing::fixtures::TempNdb;

    /// A db with sessions and their messages, written by one account.
    struct Corpus {
        ndb: Ndb,
        _dir: tempfile::TempDir,
        kp: FullKeypair,
        /// Every session note committed, whoever wrote it, subscribed before
        /// the first write.
        stream: SubscriptionStream,
        /// Every note written, so a test awaits them all committed.
        written: usize,
    }

    impl Corpus {
        fn new() -> Self {
            let TempNdb { ndb, dir } = TempNdb::new();
            let kinds = [AI_CONVERSATION_KIND as u64, AI_SESSION_STATE_KIND as u64];
            let sub = ndb
                .subscribe(&[Filter::new().kinds(kinds).build()])
                .expect("subscribe");
            let stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);
            Self {
                ndb,
                _dir: dir,
                kp: FullKeypair::generate(),
                stream,
                written: 0,
            }
        }

        /// Ingest a note built by `build`, signed by `kp`.
        fn ingest(&mut self, kp: &FullKeypair, build: NoteBuilder<'_>) {
            let note = build
                .sign(&kp.secret_key.secret_bytes())
                .build()
                .expect("note");
            let frame = nostrdb_net::ClientMessage::event(&note)
                .expect("event")
                .to_json()
                .expect("json");
            self.ndb
                .process_event_with(&frame, nostrdb::IngestMetadata::new().client(true))
                .expect("ingest");
            self.written += 1;
        }

        /// A session `id` titled `title`, with `status`.
        fn session(&mut self, id: &str, title: &str, status: &str) {
            let kp = self.kp.clone();
            let note = NoteBuilder::new()
                .kind(AI_SESSION_STATE_KIND)
                .content("")
                .created_at(1)
                .start_tag()
                .tag_str("d")
                .tag_str(id)
                .start_tag()
                .tag_str("title")
                .tag_str(title)
                .start_tag()
                .tag_str("status")
                .tag_str(status);
            self.ingest(&kp, note);
        }

        /// A message in session `id`, from `role`, at `created_at`, by `kp`.
        fn message_by(
            &mut self,
            kp: &FullKeypair,
            id: &str,
            role: &str,
            content: &str,
            created_at: u64,
        ) {
            let note = NoteBuilder::new()
                .kind(AI_CONVERSATION_KIND)
                .content(content)
                .created_at(created_at)
                .start_tag()
                .tag_str("d")
                .tag_str(id)
                .start_tag()
                .tag_str("role")
                .tag_str(role);
            self.ingest(kp, note);
        }

        /// A message the account wrote (see [`message_by`](Self::message_by)).
        fn message(&mut self, id: &str, role: &str, content: &str, created_at: u64) {
            let kp = self.kp.clone();
            self.message_by(&kp, id, role, content, created_at);
        }

        /// Wait until every note written has committed.
        async fn committed(&mut self) {
            notedeck_testing::await_notes_async(&mut self.stream, self.written).await;
        }

        /// What a scan for `query` finds, run to the end on this thread.
        fn scan(&self, query: &str) -> Vec<SessionMatch> {
            let txn = Transaction::new(&self.ndb).expect("txn");
            let mut search = Vec::new();
            let cancel = AtomicBool::new(false);
            let ran = scan(
                &self.ndb,
                &txn,
                &self.kp.pubkey,
                &pattern(query),
                &cancel,
                |found| match found {
                    Found::First(session) => search.push(session),
                    Found::Again(n) => search[n].count += 1,
                },
            );
            assert!(ran, "ran to the end");
            search
        }

        /// The hits the bar is answered with for `sessions`.
        fn answer(&self, sessions: &[SessionMatch]) -> Vec<SearchHit> {
            let txn = Transaction::new(&self.ndb).expect("txn");
            let fold = fold_sessions(&self.ndb, &txn, &self.kp.pubkey).expect("fold");
            let mut out = SearchHits::new(SCOPED_HITS);
            answer(
                sessions,
                &fold,
                &CardTitles::default(),
                &mut Localization::no_bidi(),
                &mut out,
            );
            out.hits().to_vec()
        }
    }

    /// The scan finds the account's user and assistant messages, never tool
    /// output nor another account's, one row per session with its newest
    /// match first and a count, and the bar lists them by session title.
    #[tokio::test]
    async fn a_scan_finds_messages_by_session_newest_first() {
        let mut c = Corpus::new();
        let stranger = FullKeypair::generate();
        c.session("session-old", "Old relay work", "idle");
        c.session("session-new", "Lavapipe snapshots", "working");
        c.session("session-tools", "Only tools", "idle");
        c.message("session-old", "user", "why does lavapipe crash?", 10);
        c.message("session-new", "user", "run the suite on lavapipe", 20);
        c.message(
            "session-new",
            "assistant",
            "Done.\nLavapipe passes now.\nNext?",
            30,
        );
        c.message("session-tools", "tool_result", "lavapipe: ok", 40);
        c.message("session-tools", "tool_call", "cargo test lavapipe", 41);
        c.message_by(&stranger, "session-x", "user", "lavapipe", 50);
        c.committed().await;

        let found = c.scan("lavapipe");
        let ids: Vec<&str> = found.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(ids, ["session-new", "session-old"], "newest match first");
        assert_eq!(found[0].count, 2);
        assert_eq!(found[0].line, "Lavapipe passes now.", "its newest line");

        let hits = c.answer(&found);
        let titles: Vec<&str> = hits.iter().map(|h| h.title.as_str()).collect();
        assert_eq!(titles, ["Lavapipe snapshots", "Old relay work"]);
        let hit = &hits[0];
        assert_eq!(hit.kind.as_deref(), Some(KIND_TRANSCRIPT));
        assert_eq!(
            hit.subtitle.as_deref(),
            Some("Lavapipe passes now. · 2 messages")
        );
        assert_eq!(hit.uri.reference, wordid::session_ref("session-new"));
        assert_eq!(hit.uri.at, Some(found[0].note_id), "opens at the match");
        assert_eq!(
            hits[1].subtitle.as_deref(),
            Some("why does lavapipe crash?")
        );
    }

    /// A deleted session's match is listed, marked as deleted; a session with
    /// no state event isn't, since it can't be opened.
    #[tokio::test]
    async fn a_deleted_sessions_match_is_listed_and_marked() {
        let mut c = Corpus::new();
        c.session("session-gone", "Deleted work", DELETED_STATUS);
        c.message("session-gone", "assistant", "the lavapipe fix", 10);
        c.message("session-stateless", "user", "lavapipe", 20);
        c.committed().await;

        let found = c.scan("lavapipe");
        assert_eq!(found.len(), 2, "the scan finds both");
        let hits = c.answer(&found);
        let [hit] = hits.as_slice() else {
            panic!("one hit: {hits:?}");
        };
        assert_eq!(hit.title, "Deleted work");
        assert_eq!(hit.kind.as_deref(), Some(KIND_DELETED));
        assert_eq!(hit.uri.reference, wordid::session_ref("session-gone"));
    }

    /// A cancelled scan stops at its next note, short of what it would find.
    #[tokio::test]
    async fn a_cancelled_scan_stops_at_its_next_note() {
        let mut c = Corpus::new();
        for i in 0..20 {
            c.message(&format!("session-{i}"), "user", "lavapipe", i);
        }
        c.committed().await;

        let txn = Transaction::new(&c.ndb).expect("txn");
        let cancel = AtomicBool::new(false);
        let mut found = 0;
        let ran = scan(
            &c.ndb,
            &txn,
            &c.kp.pubkey,
            &pattern("lavapipe"),
            &cancel,
            |_| {
                found += 1;
                cancel.store(true, Ordering::Relaxed);
            },
        );
        assert!(!ran, "it reports that it was cut short");
        assert_eq!(found, 1, "nothing after the cancel");
    }

    /// Asking a new query drops, and so cancels, the old one's search: what
    /// the new one lists is its own, never the old query's matches.
    #[tokio::test]
    async fn a_new_query_replaces_and_cancels_the_old_search() {
        let mut c = Corpus::new();
        c.session("session-a", "Alpha", "idle");
        c.session("session-b", "Beta", "idle");
        c.message("session-a", "user", "alpha only", 1);
        c.message("session-b", "user", "beta only", 2);
        c.committed().await;

        let now = Instant::now();
        let mut slot = None;
        let old_cancel = asked(&mut slot, &c.ndb, c.kp.pubkey, "alpha", now)
            .cancel
            .clone();
        assert!(!old_cancel.load(Ordering::Relaxed));
        let search = asked(&mut slot, &c.ndb, c.kp.pubkey, "alpha", now);
        assert!(
            Arc::ptr_eq(&search.cancel, &old_cancel),
            "the same query keeps its search"
        );

        let search = asked(&mut slot, &c.ndb, c.kp.pubkey, "beta", now);
        assert!(
            old_cancel.load(Ordering::Relaxed),
            "the old scan is cancelled"
        );
        search.finish();
        let ids: Vec<&str> = search
            .sessions()
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        assert_eq!(ids, ["session-b"]);
    }

    /// A search the bar stops asking about goes idle after [`IDLE`].
    #[test]
    fn a_search_goes_idle_once_the_bar_stops_asking() {
        let TempNdb { ndb, dir: _dir } = TempNdb::new();
        let then = Instant::now();
        let mut search = TranscriptSearch::start(&ndb, Pubkey::new([7; 32]), "q", then);
        assert!(!search.is_idle(then + IDLE / 2));
        assert!(search.is_idle(then + IDLE));
        search.poll(then + IDLE);
        assert!(!search.is_idle(then + IDLE), "asking again refreshes it");
    }

    /// A query that isn't a valid regex is searched for literally; a regex
    /// is a regex, smart-case as `agentium grep`'s.
    #[test]
    fn the_query_is_a_smart_case_regex_or_else_literal() {
        assert!(pattern("foo(").is_match("call foo(bar)"));
        assert!(pattern("lava.ipe").is_match("LAVAPIPE"));
        assert!(
            !pattern("Lavapipe").is_match("lavapipe"),
            "an upper case letter is exact"
        );
    }

    /// A long line is cut around its match with an ellipsis on each cut side.
    #[test]
    fn an_excerpt_is_cut_around_the_match() {
        let line = format!("{}needle{}", "a".repeat(100), "b".repeat(200));
        let content = format!("first line\n{line}\nlast line");
        let at = content.find("needle").expect("there");
        let cut = excerpt(&content, at..at + 6);
        assert!(cut.starts_with('…') && cut.ends_with('…'), "{cut}");
        assert!(cut.contains(&format!("{}needle", "a".repeat(CONTEXT_BEFORE))));
        assert_eq!(cut.chars().count(), EXCERPT_CHARS + 2);

        let short = "  just the needle  ";
        let at = short.find("needle").expect("there");
        assert_eq!(excerpt(short, at..at + 6), "just the needle");
    }

    /// The offer quotes the query.
    #[test]
    fn the_offer_quotes_the_query() {
        let offer = offer(&mut Localization::no_bidi(), "lavapipe");
        assert_eq!(offer.key, SCOPE);
        assert_eq!(offer.label, "Transcripts");
        assert_eq!(offer.title, "Search transcripts for “lavapipe”");
    }
}
