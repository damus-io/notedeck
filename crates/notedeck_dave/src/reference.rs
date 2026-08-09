//! The inline reference parser for an agentium session word-id —
//! `agentium:<word-id>` (e.g. `agentium:maple-river-canyon`), the same canonical
//! form a human or the `agentium`/Dave CLIs write. Registered via
//! [`App::reference_parsers`](notedeck::App::reference_parsers); the browser's
//! markdown scanner recognises the ref in a run of text
//! ([`find`](notedeck::ReferenceParser::find)),
//! [`resolve`](notedeck::ReferenceParser::resolve)s it to the session's current
//! kind-31988 state note, and draws it with the registered session renderer (see
//! [`crate::render`]).
//!
//! **Single-segment.** Like a notebook node and unlike headway's two-segment
//! `headway:<board>/<word-id>`, a session ref is just `agentium:<word-id>`: a
//! session's identity is its stable `claude_session_id` (the kind-31988 d-tag),
//! with no scope segment. So the `agentium:` scheme is the whole reference marker
//! — a bare word-id is never a reference.
//!
//! The `agentium:` scheme is **required** in free text (as in
//! [`wordid::parse_ref`](agentium_core::wordid::parse_ref)): the scheme is what
//! distinguishes a reference from ordinary `word-word-word` prose.
//! [`find`](AgentiumRefParser::find) recognises the shape cheaply and
//! allocation-free (scheme, `:`, three dash-joined words);
//! [`resolve`](AgentiumRefParser::resolve) is the authority — a candidate that
//! doesn't fold to a real session is re-rendered as plain text, so a loose match
//! is invisible, not a broken chip. Like `nostr:`/`notebook:`, the `:` survives
//! nostrdb's tokenizer, so the ref stays whole inside note content.
//!
//! **Author gap.** A session ref carries no pubkey, but folding the account's
//! sessions needs one. We resolve against
//! [`ReferenceResolveCtx::selected_account`], so a ref only resolves to a session
//! the *current* account authored — the same gap notebook's node parser has.
//!
//! **Replaceable identity.** The kind-31988 state event is replaceable, so its
//! event id churns on every update. [`resolve`](AgentiumRefParser::resolve)
//! therefore matches by re-encoding each session's stable `claude_session_id`
//! (SHA-256'd — [`encode_session_id`](agentium_core::wordid::encode_session_id),
//! *not* the raw-id `encode` headway/notebook use) and returns the note id of the
//! session's *current* state event (what the shared cache holds), so the chip
//! tracks the session as it updates.

use std::cell::RefCell;
use std::rc::Rc;

use notedeck::{ReferenceParser, ReferenceResolveCtx, ResolvedRef};

use crate::session_cache::AgentiumSessionCache;
use agentium_core::wordid;

/// The `agentium:` reference parser. Holds the app's shared
/// [`AgentiumSessionCache`] (cloned in at registration, the same `Rc` the app's
/// session renderer and per-frame pump read), so a session referenced by word id
/// resolves off the one realtime-pumped session fold — the fold happens once per
/// account, not once per surface, and a realtime edit is reflected in the
/// resolution.
pub struct AgentiumRefParser {
    cache: Rc<RefCell<AgentiumSessionCache>>,
}

impl AgentiumRefParser {
    /// Build the parser over the app's shared session cache.
    pub fn new(cache: Rc<RefCell<AgentiumSessionCache>>) -> Self {
        Self { cache }
    }

    /// A byte that would glue the scheme onto a preceding token, so `myagentium:`
    /// isn't a match latched onto the tail of a longer word: a lowercase ascii
    /// letter, a digit, or `-`.
    fn is_token_byte(b: u8) -> bool {
        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
    }
}

impl ReferenceParser for AgentiumRefParser {
    fn id(&self) -> &'static str {
        wordid::SCHEME
    }

    #[profiling::function]
    fn find(&self, text: &str) -> Option<std::ops::Range<usize>> {
        let bytes = text.as_bytes();
        let mut search = 0;
        while let Some(rel) = text[search..].find(wordid::SCHEME) {
            let start = search + rel;
            let scheme_end = start + wordid::SCHEME.len();
            // Continue past this scheme on the next iteration regardless of outcome.
            search = scheme_end;

            // The scheme must begin at a token boundary, so `myagentium:` isn't a
            // match latched onto the tail of a longer word.
            if start > 0 && Self::is_token_byte(bytes[start - 1]) {
                continue;
            }

            // The scheme is `agentium`; a full reference requires the `:` separator
            // right after it (a bare `agentium` word is not a reference).
            if scheme_end >= bytes.len() || bytes[scheme_end] != b':' {
                continue;
            }

            // Three dash-joined words right after the `:` (the shared word-id
            // grammar; see [`wordid::three_words_end`]).
            if let Some(end) = wordid::three_words_end(bytes, scheme_end + 1) {
                return Some(start..end);
            }
        }
        None
    }

    #[profiling::function]
    fn resolve(&self, matched: &str, ctx: &ReferenceResolveCtx) -> Option<ResolvedRef> {
        let words = wordid::parse_ref(matched)?;
        // Author gap: no pubkey in the ref, so fold the current account's sessions.
        let author = ctx.selected_account?;

        // Fold (memoized) the account's sessions, then re-encode each session's
        // stable id to match the word id, returning the note id of the *current*
        // state event. Resolving through the shared fold means a chip drawn right
        // after reuses this frame's fold instead of re-folding.
        let note_id = self
            .cache
            .borrow_mut()
            .resolve_wordid(ctx.ndb, ctx.txn, &author, words)?;
        Some(ResolvedRef::note(note_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_cache::session_state_filter;
    use agentium_core::session_events::AI_SESSION_STATE_KIND;
    use enostr::{FullKeypair, NoteId};
    use futures::StreamExt;
    use nostrdb::{Config, Ndb, NoteBuilder, SubscriptionStream, Transaction};

    /// A `find`-only check: the parser matches a whole `agentium:<word-id>` token
    /// and rejects the near-misses (a bare word-id, a glued scheme, a bare scheme,
    /// a fourth word). No db needed — `find` is pure syntax.
    #[test]
    fn find_matches_the_whole_token_and_rejects_near_misses() {
        let p = AgentiumRefParser::new(Rc::new(RefCell::new(AgentiumSessionCache::default())));

        let s = "see agentium:maple-river-canyon here";
        assert_eq!(&s[p.find(s).unwrap()], "agentium:maple-river-canyon");

        // A bare word-id (no scheme) is not a reference.
        assert!(p.find("maple-river-canyon").is_none());
        // The scheme glued to a preceding word is not a match.
        assert!(p.find("myagentium:maple-river-canyon").is_none());
        // A bare scheme with no words is not a match.
        assert!(p.find("agentium: trailing").is_none());
        // Only two words is not a word id.
        assert!(p.find("agentium:maple-river").is_none());
        // A fourth dash-joined word is rejected (not a bare three-word id).
        assert!(p.find("agentium:maple-river-canyon-extra").is_none());
        // Prose with no reference yields nothing.
        assert!(p.find("just some notes").is_none());
    }

    /// A headless harness driving an [`AgentiumSessionCache`] against a bare `Ndb`
    /// so the parser resolves off the same shared fold the app pumps.
    struct TestParser {
        ndb: Ndb,
        _dir: tempfile::TempDir,
        kp: FullKeypair,
        cache: Rc<RefCell<AgentiumSessionCache>>,
        stream: SubscriptionStream,
    }

    impl TestParser {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
            let kp = FullKeypair::generate();
            let sub = ndb.subscribe(&[session_state_filter(&kp.pubkey)]).unwrap();
            let stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);
            Self {
                ndb,
                _dir: dir,
                kp,
                cache: Rc::new(RefCell::new(AgentiumSessionCache::default())),
                stream,
            }
        }

        /// Write a kind-31988 session-state event and ingest it locally.
        fn write_state(&self, session_id: &str, title: &str) {
            let note = NoteBuilder::new()
                .kind(AI_SESSION_STATE_KIND)
                .content("")
                .created_at(1_000)
                .start_tag()
                .tag_str("d")
                .tag_str(session_id)
                .start_tag()
                .tag_str("title")
                .tag_str(title)
                .start_tag()
                .tag_str("status")
                .tag_str("working")
                .sign(&self.kp.secret_key.secret_bytes())
                .build()
                .unwrap();
            let frame = enostr::ClientMessage::event(&note)
                .unwrap()
                .to_json()
                .unwrap();
            self.ndb
                .process_event_with(&frame, nostrdb::IngestMetadata::new().client(true))
                .unwrap();
        }

        /// Await one committed note, then pump the shared cache so it folds it in.
        async fn await_and_poll(&mut self) {
            self.stream.next().await.expect("subscription open");
            let txn = Transaction::new(&self.ndb).unwrap();
            self.cache
                .borrow_mut()
                .poll(&self.ndb, &txn, &self.kp.pubkey);
        }

        /// The kind-31988 note id currently stored for `session_id`.
        fn note_id(&mut self, session_id: &str) -> Option<NoteId> {
            let txn = Transaction::new(&self.ndb).unwrap();
            self.cache
                .borrow_mut()
                .session(&self.ndb, &txn, &self.kp.pubkey, session_id)
                .map(|v| v.note_id)
        }
    }

    /// End-to-end: an `agentium:<word-id>` for a real session on the account
    /// resolves (through the shared cache) to that session's current kind-31988
    /// state note id; an unknown word-id, and a resolve for a different account,
    /// both miss.
    #[tokio::test]
    async fn resolve_maps_a_session_wordid_to_its_state_note() {
        let mut t = TestParser::new();
        let sid = "session-gamma";
        t.write_state(sid, "Gamma");
        let note_id = loop {
            t.await_and_poll().await;
            if let Some(id) = t.note_id(sid) {
                break id;
            }
        };

        let parser = AgentiumRefParser::new(t.cache.clone());
        let matched = agentium_core::wordid::session_ref(sid);
        let txn = Transaction::new(&t.ndb).unwrap();
        let ctx = ReferenceResolveCtx {
            ndb: &t.ndb,
            txn: &txn,
            selected_account: Some(t.kp.pubkey),
        };

        // The real session resolves to its current state note id.
        let resolved = parser.resolve(&matched, &ctx).expect("session resolves");
        assert_eq!(resolved.note_id, note_id);

        // A well-formed but unknown word-id folds to nothing (drawn as plain text).
        let unknown = format!("{}:maple-river-canyon", wordid::SCHEME);
        assert!(parser.resolve(&unknown, &ctx).is_none());

        // The author gap: the same ref resolved for a *different* account misses,
        // since that account has no such session folded.
        let other = FullKeypair::generate();
        let ctx_other = ReferenceResolveCtx {
            ndb: &t.ndb,
            txn: &txn,
            selected_account: Some(other.pubkey),
        };
        assert!(parser.resolve(&matched, &ctx_other).is_none());
    }
}
