//! Dave's answer to the chrome's universal search (the Cmd-K bar, see
//! [`notedeck::search`]): the account's agent sessions, on every host, by
//! title, by reference and by the issue they were spawned for.
//!
//! The sessions are the shared session fold's live views (the
//! [`AgentiumSessionCache`](crate::session_cache::AgentiumSessionCache) the
//! inline `agentium:` chips read), so deleted sessions are already gone and a
//! remote host's session is found the same as a local one. A session matches:
//!
//! - **by reference**: the query is its `agentium:<word-id>` (or the bare
//!   word-id, looked up in the fold's word-id index by the caller), its
//!   session id, or its CLI session id, exactly; or a prefix of either id;
//! - **by issue**: the query is the `headway:` reference it was spawned for
//!   (`--issue-url`), whole, without its scheme, or just the card's word-id,
//!   so a card finds the sessions that worked it. Not a part of it: a board's
//!   name would list every session spawned for that board's cards;
//! - **by title**: the shared [`fuzzy_match`] on its display title, and on
//!   the current title of what its title or issue references (the card a
//!   `headway:<board>/<word-id>` names), so a session titled with its card's
//!   ref is found by the card's words. A hit titled with a ref that resolved
//!   shows the card's title, with the ref before its subtitle.
//!
//! Those titles are the ones the session list keeps
//! ([`CardTitles`]), looked up through the registered reference parsers and
//! kind renderers, so Dave never learns what a `headway:` ref is. The search
//! refreshes them first, in two steps: [`stale_refs`] while the session fold
//! is borrowed, then [`refresh_titles`] once it isn't, since resolving an
//! `agentium:` ref reads that same fold. Checking whether a kept title still
//! holds reads only the revisions of what it was looked up from, which for
//! an `agentium:` ref is nothing (its parser offers none), so that check can
//! run under the borrow.
//!
//! Hits rank by that score, then by last activity, newest first. Each opens
//! through its `agentium:<word-id>` reference, which the app's
//! [`AgentiumRefParser`](crate::reference::AgentiumRefParser) resolves to its
//! state note and the chrome routes to Dave, the way a click on the session's
//! inline chip is.
//!
//! Message text is not searched (as `agentium grep` does): that needs a
//! full-text query per keystroke, on a worker.

use agentium_core::session_loader::SessionState;
use agentium_core::wordid;
use nostrdb::Transaction;
use nostrdb_net::{NoteId, Pubkey};
use notedeck::fuzzy::fuzzy_match;
use notedeck::{
    AppContext, AppRegistries, OpenUri, ReferenceParserRegistry, ReferenceResolveCtx, SearchHit,
    SearchHits, SearchQuery,
};

use crate::card_titles::CardTitles;
use crate::path_utils::abbreviate_with_home;
use crate::session_cache::SessionView;

/// What a session hit is marked as in the bar.
const KIND_SESSION: &str = "Session";

/// The score of a query that names the session exactly, by reference or id:
/// above any title match, so the session it names lists first.
const EXACT: i32 = 1 << 20;

/// The score of a query that begins one of the session's ids.
const ID_PREFIX: i32 = 1 << 19;

/// The score of a query naming the issue the session was spawned for.
const ISSUE: i32 = 1 << 18;

/// The shortest id prefix that matches: a UUID's first group, so a word
/// spelled in hex (`decade`) isn't taken for one.
const MIN_ID_PREFIX: usize = 8;

/// One session that matched, before the cut to the provider's cap.
struct Ranked {
    /// Its index in the sessions searched.
    at: usize,
    /// How well it matched (see the module docs).
    score: i32,
    /// When it was last active, breaking ties in `score`, newest first.
    last_activity: u64,
}

impl crate::Dave {
    /// Bring the kept titles of what the account's sessions reference up to
    /// date for a search (see the module docs), regrouping the session list
    /// when one changed. Skipped without a window to read the time from: the
    /// search then answers from the titles already kept.
    pub(crate) fn refresh_search_titles(
        &mut self,
        ctx: &AppContext<'_>,
        txn: &Transaction,
        author: Pubkey,
    ) {
        let Some(now) = ctx.egui.map(|egui| egui.input(|i| i.time)) else {
            return;
        };
        let titles = &mut self.session_manager.card_titles;
        let resolve = ReferenceResolveCtx {
            ndb: ctx.ndb,
            txn,
            selected_account: Some(author),
        };
        // Collected under the fold's borrow, resolved after it drops.
        let stale = self
            .session_cache
            .borrow_mut()
            .with_sessions(ctx.ndb, txn, &author, |sessions| {
                stale_refs(sessions, ctx.registries, &resolve, titles, now)
            })
            .unwrap_or_default();
        if stale.is_empty() {
            return;
        }
        if refresh_titles(&stale, ctx.registries, &resolve, titles, now) {
            self.session_manager.lineage_changed();
        }
    }
}

/// The references among `sessions`' titles and issues (each whole, trimmed)
/// whose lookup kept in `titles` no longer holds at `now` (see
/// [`CardTitles::is_current`]), or that have none: the ones
/// [`refresh_titles`] looks up. Only strings that are exactly one reference
/// a registered parser recognizes, so a plain title is never looked up.
#[profiling::function]
pub(crate) fn stale_refs(
    sessions: &[SessionView],
    registries: &AppRegistries,
    ctx: &ReferenceResolveCtx,
    titles: &CardTitles,
    now: f64,
) -> Vec<String> {
    let parsers: &ReferenceParserRegistry = &registries.reference_parsers;
    let mut stale = Vec::new();
    for view in sessions {
        let state = &view.state;
        if state.is_deleted() {
            continue;
        }
        let issue = state.issue_url.as_deref();
        for text in std::iter::once(state.display_title()).chain(issue) {
            let text = text.trim();
            if parsers.is_reference(text) && !titles.is_current_in(registries, ctx, text, now) {
                stale.push(text.to_owned());
            }
        }
    }
    stale
}

/// Look up each of `refs` into `titles` through `registries`, at `now`,
/// skipping one whose kept lookup already holds (a ref two sessions share is
/// listed twice). Returns whether what any lookup found changed, so the
/// caller regroups the session list (see [`CardTitles::store`]).
#[profiling::function]
pub(crate) fn refresh_titles(
    refs: &[String],
    registries: &AppRegistries,
    ctx: &ReferenceResolveCtx,
    titles: &mut CardTitles,
    now: f64,
) -> bool {
    let mut changed = false;
    for r in refs {
        changed |= titles.refresh(registries, ctx, r, now);
    }
    changed
}

/// Answer `query` from `sessions`, the account's live session views, into
/// `out`, best first, up to its cap. `by_word_id` is the state note of the
/// session whose word-id the query is, if any, from the fold's index.
/// `titles` holds the titles of what sessions' titles and issues reference.
#[profiling::function]
pub(crate) fn search(
    sessions: &[SessionView],
    query: &SearchQuery,
    by_word_id: Option<NoteId>,
    titles: &CardTitles,
    out: &mut SearchHits,
) {
    let text = query.text;
    let mut positions = Vec::new();
    let mut ranked: Vec<Ranked> = Vec::new();
    for (at, view) in sessions.iter().enumerate() {
        let state = &view.state;
        if state.is_deleted() {
            continue;
        }
        let named = by_word_id == Some(view.note_id);
        let Some(score) = score(state, named, text, titles, &mut positions) else {
            continue;
        };
        ranked.push(Ranked {
            at,
            score,
            last_activity: view.last_activity,
        });
    }

    ranked.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.last_activity.cmp(&a.last_activity))
    });
    for r in ranked {
        if out.is_full() {
            break;
        }
        out.push(hit(&sessions[r.at].state, titles, r.score));
    }
}

/// How well `text` matches `state`, the best of its ways to match (see the
/// module docs), or `None` when it doesn't. `named` is whether the query is
/// the session's word-id.
fn score(
    state: &SessionState,
    named: bool,
    text: &str,
    titles: &CardTitles,
    positions: &mut Vec<usize>,
) -> Option<i32> {
    let cli = state.cli_session_id.as_deref();
    if named || state.claude_session_id == text || cli == Some(text) {
        return Some(EXACT);
    }
    let id_prefix = text.len() >= MIN_ID_PREFIX
        && (state.claude_session_id.starts_with(text) || cli.is_some_and(|c| c.starts_with(text)));
    if id_prefix {
        return Some(ID_PREFIX);
    }
    if state
        .issue_url
        .as_deref()
        .is_some_and(|issue| names_issue(issue, text))
    {
        return Some(ISSUE);
    }
    let title = state.display_title();
    let by_title = fuzzy_match(text, title, positions);
    let mut by_ref = |r: &str| fuzzy_match(text, ref_title(titles, r)?, positions);
    let by_title_ref = by_ref(title);
    let by_issue = state.issue_url.as_deref().and_then(by_ref);
    by_title.max(by_title_ref).max(by_issue)
}

/// The title kept for what `text` references, when the whole of it is a
/// reference that resolved.
fn ref_title<'a>(titles: &'a CardTitles, text: &str) -> Option<&'a str> {
    titles.get(text.trim())
}

/// Whether `text` names `issue`, a `headway:<board>/<word-id>` reference:
/// the whole of it, the part after its scheme, or the part after its board.
fn names_issue(issue: &str, text: &str) -> bool {
    let after = |sep| issue.split_once(sep).is_some_and(|(_, rest)| rest == text);
    issue == text || after(':') || after('/')
}

/// The bar's row for `state`: its title, marked as a session, with where and
/// how it runs, opening its `agentium:` reference. A title that is a
/// reference shows what it references, by its title in `titles`, with the
/// reference itself leading the subtitle.
fn hit(state: &SessionState, titles: &CardTitles, score: i32) -> SearchHit {
    let uri = OpenUri::new(wordid::session_ref(&state.claude_session_id));
    let cwd = abbreviate_with_home(std::path::Path::new(&state.cwd), &state.home_dir);
    let place = format!("{} · {} · {}", state.hostname, cwd, state.status);
    let title = state.display_title();
    let Some(referenced) = ref_title(titles, title) else {
        return SearchHit::new(title, uri, score)
            .kind(KIND_SESSION)
            .subtitled(place);
    };
    SearchHit::new(referenced, uri, score)
        .kind(KIND_SESSION)
        .subtitled(format!("{} · {place}", title.trim()))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use super::*;
    use crate::card_titles::{CardLookup, UNVERSIONED_REFRESH_SECS};
    use crate::test_refs::{ingest_card, StubCardParser, StubCardRenderer};
    use notedeck::search::HITS_PER_PROVIDER;
    use notedeck_testing::fixtures::TempNdb;

    /// A live session `id` titled `title`, last active at `last_activity`.
    fn view(id: &str, title: &str, last_activity: u64) -> SessionView {
        // Distinct per session: the tests' ids differ within 32 bytes.
        let mut note_id = [0u8; 32];
        for (b, c) in note_id.iter_mut().zip(id.bytes()) {
            *b = c;
        }
        SessionView {
            note_id: NoteId::new(note_id),
            state: SessionState {
                claude_session_id: id.to_owned(),
                title: title.to_owned(),
                cwd: "/home/jb55/dev/notedeck".to_owned(),
                home_dir: "/home/jb55".to_owned(),
                hostname: "monad".to_owned(),
                status: "working".to_owned(),
                ..SessionState::default()
            },
            last_activity,
        }
    }

    /// The titles `query` finds in `sessions`, best first.
    fn found(sessions: &[SessionView], query: &str, by_word_id: Option<NoteId>) -> Vec<String> {
        found_titled(sessions, query, by_word_id, &CardTitles::default())
    }

    /// The titles `query` finds in `sessions`, with `titles` kept for what
    /// they reference, best first.
    fn found_titled(
        sessions: &[SessionView],
        query: &str,
        by_word_id: Option<NoteId>,
        titles: &CardTitles,
    ) -> Vec<String> {
        let mut out = SearchHits::new(HITS_PER_PROVIDER);
        search(
            sessions,
            &SearchQuery { text: query },
            by_word_id,
            titles,
            &mut out,
        );
        out.hits().iter().map(|h| h.title.clone()).collect()
    }

    /// A title query finds its session as a hit that opens its `agentium:`
    /// reference, subtitled with host, abbreviated cwd and status, and
    /// misses the sessions it doesn't match.
    #[test]
    fn a_title_query_finds_its_session_and_opens_its_reference() {
        let sessions = [
            view("session-relay", "Fix relay reconnect", 10),
            view("session-cmdk", "Cmd-K session search", 20),
        ];
        let mut out = SearchHits::new(HITS_PER_PROVIDER);
        search(
            &sessions,
            &SearchQuery {
                text: "relay reconnect",
            },
            None,
            &CardTitles::default(),
            &mut out,
        );
        let [hit] = out.hits() else {
            panic!("one hit: {:?}", out.hits());
        };
        assert_eq!(hit.title, "Fix relay reconnect");
        assert_eq!(hit.kind.as_deref(), Some("Session"));
        assert_eq!(
            hit.subtitle.as_deref(),
            Some("monad · ~/dev/notedeck · working")
        );
        assert_eq!(
            hit.uri.reference,
            wordid::session_ref("session-relay"),
            "opens the session's agentium: reference"
        );
    }

    /// A custom title is the one searched and shown.
    #[test]
    fn a_custom_title_is_searched_and_shown() {
        let mut renamed = view("session-a", "claude's own title", 1);
        renamed.state.custom_title = Some("Renamed by hand".to_owned());
        assert_eq!(found(&[renamed], "renamed", None), ["Renamed by hand"]);
    }

    /// A session named by reference lists first, above a title match:
    /// by word-id (as the fold's index finds it), by session id, by CLI
    /// session id, or by a prefix of an id.
    #[test]
    fn a_reference_or_id_lists_its_session_first() {
        let mut target = view("8f2c1d9e-aaaa", "Unrelated title", 1);
        target.state.cli_session_id = Some("cli-77ab31".to_owned());
        let decoy = view("session-decoy", "8f2c 8f2c cli-77ab31 decoy", 100);
        let sessions = [decoy, target];
        let note = sessions[1].note_id;

        assert_eq!(
            found(&sessions, "maple-river-canyon", Some(note))[0],
            "Unrelated title",
            "by word-id"
        );
        assert_eq!(
            found(&sessions, "8f2c1d9e-aaaa", None)[0],
            "Unrelated title"
        );
        assert_eq!(found(&sessions, "cli-77ab31", None)[0], "Unrelated title");
        assert_eq!(found(&sessions, "8f2c1d9e", None)[0], "Unrelated title");
    }

    /// The issue a session was spawned for finds it, by the card's whole
    /// reference, the reference without its scheme, or its word-id, but not
    /// by any other part of it.
    #[test]
    fn the_issue_it_was_spawned_for_finds_a_session() {
        let mut worked = view("session-card", "Dave sessions in Cmd-K", 1);
        worked.state.issue_url = Some("headway:notedeck/label-weekend-expire".to_owned());
        let other = view("session-other", "Something else", 2);
        let sessions = [worked, other];
        assert_eq!(
            found(&sessions, "headway:notedeck/label-weekend-expire", None),
            ["Dave sessions in Cmd-K"]
        );
        assert_eq!(
            found(&sessions, "notedeck/label-weekend-expire", None),
            ["Dave sessions in Cmd-K"]
        );
        assert_eq!(
            found(&sessions, "label-weekend-expire", None),
            ["Dave sessions in Cmd-K"]
        );
        // The board's name is part of the ref, but names no one issue.
        assert!(found(&sessions, "notedeck", None).is_empty());
    }

    /// A deleted session is never a hit, even by its exact id.
    #[test]
    fn a_deleted_session_is_not_found() {
        let mut gone = view("session-gone", "Deleted session", 1);
        gone.state.status = agentium_core::session_loader::DELETED_STATUS.to_owned();
        let note = gone.note_id;
        let sessions = [gone];
        assert!(found(&sessions, "deleted", None).is_empty());
        assert!(found(&sessions, "session-gone", Some(note)).is_empty());
    }

    /// Equal matches rank the most recently active first, and the answer
    /// stops at the provider's cap.
    #[test]
    fn ties_rank_by_last_activity_and_stop_at_the_cap() {
        let sessions: Vec<SessionView> = (0..HITS_PER_PROVIDER as u64 + 3)
            .map(|i| view(&format!("session-{i}"), "Review pass", i))
            .collect();
        let mut out = SearchHits::new(HITS_PER_PROVIDER);
        search(
            &sessions,
            &SearchQuery { text: "review" },
            None,
            &CardTitles::default(),
            &mut out,
        );
        assert_eq!(out.hits().len(), HITS_PER_PROVIDER);
        let newest = wordid::session_ref(&format!("session-{}", HITS_PER_PROVIDER + 2));
        assert_eq!(out.hits()[0].uri.reference, newest);
    }

    /// A session titled with a card's ref is found by the card's title words,
    /// shown under the card's title with the ref leading its subtitle, and is
    /// still found by its ref.
    #[test]
    fn a_ref_titled_session_is_found_and_shown_by_its_cards_title() {
        let card_ref = "headway:notedeck/field-fever-follow";
        let sessions = [
            view("session-card", card_ref, 1),
            view("session-other", "Something else", 2),
        ];
        let mut titles = CardTitles::default();
        titles.store(
            card_ref,
            CardLookup::titled("Find sessions by card title"),
            0.0,
        );

        let mut out = SearchHits::new(HITS_PER_PROVIDER);
        let query = SearchQuery {
            text: "sessions card title",
        };
        search(&sessions, &query, None, &titles, &mut out);
        let [hit] = out.hits() else {
            panic!("one hit: {:?}", out.hits());
        };
        assert_eq!(hit.title, "Find sessions by card title");
        assert_eq!(
            hit.subtitle.as_deref(),
            Some("headway:notedeck/field-fever-follow · monad · ~/dev/notedeck · working")
        );
        assert_eq!(hit.uri.reference, wordid::session_ref("session-card"));

        assert_eq!(
            found_titled(&sessions, "field-fever", None, &titles),
            ["Find sessions by card title"],
            "the ref stays matchable"
        );
        assert!(
            found(&sessions, "sessions card title", None).is_empty(),
            "nothing finds it by a title not looked up"
        );
    }

    /// A session spawned for a card is found by the card's title words too,
    /// and keeps its own title.
    #[test]
    fn the_issues_card_title_finds_a_session() {
        let card_ref = "headway:notedeck/label-weekend-expire";
        let mut worked = view("session-card", "Dave sessions in Cmd-K", 1);
        worked.state.issue_url = Some(card_ref.to_owned());
        let mut titles = CardTitles::default();
        titles.store(
            card_ref,
            CardLookup::titled("Search provider for agents"),
            0.0,
        );
        assert_eq!(
            found_titled(&[worked], "provider agents", None, &titles),
            ["Dave sessions in Cmd-K"]
        );
    }

    /// The refs a search looks up are the sessions' titles and issues that
    /// are references whose kept lookup no longer holds: not a plain title,
    /// not one looked up within the window (the stand-in parser versions
    /// nothing), not a deleted session's.
    #[test]
    fn stale_refs_are_the_references_not_looked_up_lately() {
        let TempNdb { ndb, dir: _dir } = TempNdb::new();
        let txn = nostrdb::Transaction::new(&ndb).expect("txn");
        let ctx = ReferenceResolveCtx {
            ndb: &ndb,
            txn: &txn,
            selected_account: None,
        };
        let mut registries = AppRegistries::default();
        registries
            .reference_parsers
            .register(Box::new(StubCardParser {
                card: Default::default(),
            }));
        let mut titled = view("session-a", "headway:notedeck/field-fever-follow", 1);
        titled.state.issue_url = Some("headway:notedeck/label-weekend-expire".to_owned());
        let plain = view("session-b", "A plain title", 2);
        let mut gone = view("session-c", "headway:notedeck/gone-gone-gone", 3);
        gone.state.status = agentium_core::session_loader::DELETED_STATUS.to_owned();
        let sessions = [titled, plain, gone];

        let mut titles = CardTitles::default();
        assert_eq!(
            stale_refs(&sessions, &registries, &ctx, &titles, 100.0),
            [
                "headway:notedeck/field-fever-follow",
                "headway:notedeck/label-weekend-expire"
            ]
        );
        titles.store(
            "headway:notedeck/field-fever-follow",
            CardLookup::default(),
            90.0,
        );
        assert_eq!(
            stale_refs(&sessions, &registries, &ctx, &titles, 100.0),
            ["headway:notedeck/label-weekend-expire"],
            "a ref looked up within the window, even a miss, isn't again"
        );
        assert_eq!(
            stale_refs(
                &sessions,
                &registries,
                &ctx,
                &titles,
                90.0 + UNVERSIONED_REFRESH_SECS
            )
            .len(),
            2,
            "until the window passes"
        );
    }

    /// The whole path: a session titled with a card's ref, and the card's
    /// note in the db, resolved through the registered parser and renderer
    /// (stand-ins for headway's). A query of the card's title words finds the
    /// session.
    #[tokio::test]
    async fn a_query_of_its_cards_title_finds_a_ref_titled_session() {
        let card_ref = "headway:notedeck/field-fever-follow";
        let card_title = "Find Dave sessions by the title of the card they reference";
        let TempNdb { ndb, dir: _dir } = TempNdb::new();
        let card = Rc::new(RefCell::new(None));
        let mut registries = AppRegistries::default();
        registries
            .reference_parsers
            .register(Box::new(StubCardParser { card: card.clone() }));
        registries
            .kind_renderers
            .register(Box::new(StubCardRenderer));

        let kp = nostrdb_net::FullKeypair::generate();
        let sub = ndb
            .subscribe(&[nostrdb::Filter::new().kinds([1621]).build()])
            .expect("subscribe");
        *card.borrow_mut() = Some(ingest_card(
            &ndb,
            &kp.secret_key.secret_bytes(),
            card_ref,
            card_title,
        ));
        tokio::time::timeout(Duration::from_secs(5), ndb.wait_for_notes(sub, 1))
            .await
            .expect("ingested within 5s")
            .expect("ingest notified the subscription");

        let sessions = [
            view("session-card", card_ref, 1),
            view("session-other", "Fix relay reconnect", 2),
        ];
        let mut titles = CardTitles::default();
        let txn = nostrdb::Transaction::new(&ndb).expect("txn");
        let ctx = ReferenceResolveCtx {
            ndb: &ndb,
            txn: &txn,
            selected_account: None,
        };
        let stale = stale_refs(&sessions, &registries, &ctx, &titles, 0.0);
        assert!(refresh_titles(&stale, &registries, &ctx, &mut titles, 0.0));
        assert!(
            !refresh_titles(&stale, &registries, &ctx, &mut titles, 1.0),
            "nothing is looked up again inside the window"
        );

        assert_eq!(
            found_titled(&sessions, "title of the card", None, &titles),
            [card_title]
        );
    }
}
