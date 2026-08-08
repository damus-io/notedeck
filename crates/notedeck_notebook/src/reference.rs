//! The inline reference parser for a notebook node word-id — `notebook:<word-id>`
//! (e.g. `notebook:maple-river-canyon`), the same canonical form a human or the
//! `notebook` CLI writes. Registered via
//! [`App::reference_parsers`](notedeck::App::reference_parsers); the browser's
//! markdown scanner recognises the ref in a run of text
//! ([`find`](notedeck::ReferenceParser::find)),
//! [`resolve`](notedeck::ReferenceParser::resolve)s it to the node's kind-1606
//! creation note, and draws it with the registered node renderer (see
//! [`crate::render`]).
//!
//! **Single-segment.** Unlike headway's two-segment `headway:<board>/<word-id>`, a
//! notebook ref is just `notebook:<word-id>`: a node's identity is its 32-byte
//! creation event id, and the canvas it currently sits on is *not* part of that
//! identity (see [`crate::wordid`]). So the `notebook:` scheme is the whole
//! reference marker — a bare word-id is never a reference — and there is no
//! board/canvas segment between the scheme and the words.
//!
//! The `notebook:` scheme is **required** in free text (as in [`wordid::parse_ref`]):
//! the scheme is what distinguishes a reference from ordinary `word-word-word`
//! prose. [`find`](NotebookRefParser::find) recognises the shape cheaply and
//! allocation-free (scheme, `:`, three dash-joined words);
//! [`resolve`](NotebookRefParser::resolve) is the authority — a candidate that
//! doesn't fold to a real node is re-rendered as plain text, so a loose match is
//! invisible, not a broken chip. Like `nostr:`/`headway:`, the `:` survives
//! nostrdb's tokenizer, so the ref stays whole inside note content.
//!
//! **Author gap.** A node ref carries no pubkey, but folding a canvas needs one.
//! We resolve against [`ReferenceResolveCtx::selected_account`], so a ref only
//! resolves to a node the *current* account authored. A later grammar extension
//! can carry an explicit author (e.g. an `naddr`-style coordinate).

use std::cell::RefCell;
use std::rc::Rc;

use enostr::NoteId;
use notedeck::{ReferenceParser, ReferenceResolveCtx, ResolvedRef};

use crate::NotebookCache;
use crate::event::CanvasView;
use crate::wordid;

/// The `notebook:` reference parser. Holds the app's shared
/// [`NotebookCache`](crate::NotebookCache) (cloned in at registration, the same
/// `Rc` the app's kind renderers and per-frame pump read), so a node referenced by
/// word id resolves off the one realtime-pumped canvas fold — the fold happens once
/// per account, not once per surface, and a realtime edit is reflected in the
/// resolution.
pub(crate) struct NotebookRefParser {
    cache: Rc<RefCell<NotebookCache>>,
}

impl NotebookRefParser {
    /// Build the parser over the app's shared canvas cache.
    pub(crate) fn new(cache: Rc<RefCell<NotebookCache>>) -> Self {
        Self { cache }
    }

    /// A byte that would glue the scheme onto a preceding token, so `mynotebook:`
    /// isn't a match latched onto the tail of a longer word: a lowercase ascii
    /// letter, a digit, or `-`.
    fn is_token_byte(b: u8) -> bool {
        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
    }
}

impl ReferenceParser for NotebookRefParser {
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

            // The scheme must begin at a token boundary, so `mynotebook:` isn't a
            // match latched onto the tail of a longer word.
            if start > 0 && Self::is_token_byte(bytes[start - 1]) {
                continue;
            }

            // The scheme is `notebook`; a full reference requires the `:` separator
            // right after it (a bare `notebook` word is not a reference).
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
        // Author gap: no pubkey in the ref, so fold the current account's canvases.
        let author = ctx.selected_account?;

        // Fold (memoized) the account's canvases, then re-encode each node's id to
        // match the word id. The closure yields the owned `NoteId` so the cache
        // borrow drops here, and resolving through the shared finalize means a chip
        // drawn right after reuses this frame's fold instead of re-folding.
        let note_id = self
            .cache
            .borrow_mut()
            .with_canvases(ctx.ndb, ctx.txn, &author, |canvases| {
                resolve_node_by_wordid(canvases, words)
            })
            .flatten()?;
        Some(ResolvedRef::note(note_id))
    }
}

/// The creation-event id of the node whose word-id is `words`, across every canvas
/// in `canvases` (pending nodes included, mirroring the CLI's `find_node`). A
/// node's identity is its creation event id, so each candidate is matched by
/// re-encoding that id — exactly how a git short hash is resolved. `None` if no
/// node matches.
fn resolve_node_by_wordid(canvases: &[CanvasView], words: &str) -> Option<NoteId> {
    canvases
        .iter()
        .flat_map(|c| c.nodes.iter().chain(c.pending.iter()))
        .find(|n| wordid::encode(n.id.bytes()) == words)
        .map(|n| n.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event;
    use crate::store::{self, CANVAS_ID, CanvasAction, NoPublish};
    use enostr::FullKeypair;
    use futures_util::StreamExt;
    use nostrdb::{Config, Ndb, SubscriptionStream, Transaction};

    /// A `find`-only check: the parser matches a whole `notebook:<word-id>` token
    /// and rejects the near-misses (a bare word-id, a glued scheme, a bare
    /// scheme, a fourth word). No db needed — `find` is pure syntax.
    #[test]
    fn find_matches_the_whole_token_and_rejects_near_misses() {
        let p = NotebookRefParser::new(Rc::new(RefCell::new(NotebookCache::default())));

        let s = "see notebook:maple-river-canyon here";
        assert_eq!(&s[p.find(s).unwrap()], "notebook:maple-river-canyon");

        // A bare word-id (no scheme) is not a reference.
        assert!(p.find("maple-river-canyon").is_none());
        // The scheme glued to a preceding word is not a match.
        assert!(p.find("mynotebook:maple-river-canyon").is_none());
        // A bare scheme with no words is not a match.
        assert!(p.find("notebook: trailing").is_none());
        // Only two words is not a word id.
        assert!(p.find("notebook:maple-river").is_none());
        // A fourth dash-joined word is rejected (not a bare three-word id).
        assert!(p.find("notebook:maple-river-canyon-extra").is_none());
        // Prose with no reference yields nothing.
        assert!(p.find("just some notes").is_none());
    }

    /// A headless harness driving a [`NotebookCache`] against a bare `Ndb` so the
    /// parser resolves off the same shared fold the app pumps. Mirrors the lib
    /// tests' `TestCache`; ingest is async, so each write awaits a side
    /// subscription before the cache is polled.
    struct TestParser {
        ndb: Ndb,
        _dir: tempfile::TempDir,
        kp: FullKeypair,
        cache: Rc<RefCell<NotebookCache>>,
        stream: SubscriptionStream,
    }

    impl TestParser {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
            let kp = FullKeypair::generate();
            let sub = ndb
                .subscribe(&[event::notebook_filter(&kp.pubkey)])
                .unwrap();
            let stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);
            Self {
                ndb,
                _dir: dir,
                kp,
                cache: Rc::new(RefCell::new(NotebookCache::default())),
                stream,
            }
        }

        fn secret(&self) -> [u8; 32] {
            self.kp.secret_key.secret_bytes()
        }

        /// Await `n` committed notes on the side subscription.
        fn await_notes(&mut self, n: usize) {
            pollster::block_on(async {
                let mut seen = 0;
                while seen < n {
                    seen += self.stream.next().await.expect("subscription open").len();
                }
            });
        }

        /// Pump the shared cache under a fresh read txn.
        fn poll(&mut self) {
            let txn = Transaction::new(&self.ndb).unwrap();
            self.cache
                .borrow_mut()
                .poll(&self.ndb, &txn, &self.kp.pubkey);
        }

        /// Resolve the managed canvas under a fresh read txn.
        fn canvas(&mut self) -> Option<CanvasView> {
            let txn = Transaction::new(&self.ndb).unwrap();
            self.cache
                .borrow_mut()
                .canvas(&self.ndb, &txn, &self.kp.pubkey, CANVAS_ID)
        }

        fn add_node(&mut self, view: &CanvasView, label: &str) {
            store::apply(
                &self.ndb,
                CANVAS_ID,
                view,
                &self.kp.pubkey,
                &self.secret(),
                CanvasAction::AddNode {
                    kind: event::NodeKind::Text,
                    geo: event::Geometry {
                        x: 0,
                        y: 0,
                        w: 200,
                        h: 80,
                    },
                    content: event::NodeContent {
                        text: label.to_string(),
                        ..Default::default()
                    },
                },
                &mut NoPublish,
            );
        }
    }

    /// End-to-end: a `notebook:<word-id>` for a real node on the account's canvas
    /// resolves (through the shared cache) to that node's kind-1606 creation event
    /// id; a word-id for no node, and a resolve for a different account, both miss.
    #[test]
    fn resolve_maps_a_node_wordid_to_its_creation_note() {
        let mut t = TestParser::new();
        store::seed_canvas(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            CANVAS_ID,
            "Canvas",
            &mut NoPublish,
        );
        t.await_notes(1);
        t.poll();
        let view = t.canvas().expect("canvas seeded");

        // Add a node and fold it in.
        t.add_node(&view, "hello");
        t.await_notes(2);
        let node_id = loop {
            t.poll();
            if let Some(v) = t.canvas()
                && let Some(node) = v.nodes.first()
            {
                break node.id;
            }
        };

        let parser = NotebookRefParser::new(t.cache.clone());
        let matched = wordid::node_ref(node_id.bytes());
        let txn = Transaction::new(&t.ndb).unwrap();
        let ctx = ReferenceResolveCtx {
            ndb: &t.ndb,
            txn: &txn,
            selected_account: Some(t.kp.pubkey),
        };

        // The real node resolves to its creation event id.
        let resolved = parser.resolve(&matched, &ctx).expect("node resolves");
        assert_eq!(resolved.note_id, node_id);

        // A well-formed but unknown word-id folds to nothing (drawn as plain text).
        let unknown = format!("{}:maple-river-canyon", wordid::SCHEME);
        assert!(parser.resolve(&unknown, &ctx).is_none());

        // The author gap: the same ref resolved for a *different* account misses,
        // since that account has no such node folded.
        let other = FullKeypair::generate();
        let ctx_other = ReferenceResolveCtx {
            ndb: &t.ndb,
            txn: &txn,
            selected_account: Some(other.pubkey),
        };
        assert!(parser.resolve(&matched, &ctx_other).is_none());
    }
}
