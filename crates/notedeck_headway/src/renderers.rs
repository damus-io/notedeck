//! Inline renderers — drawing a single headway entity referenced from elsewhere
//! (e.g. a `nostr:` link in a notebook note), via notedeck's `KindRenderer`
//! registry, plus the `headway:<board>/<word-id>` [`ReferenceParser`](notedeck::ReferenceParser).
//! These are read-only and self-contained, unlike the editable board.

use std::cell::RefCell;
use std::rc::Rc;

use nostrdb_net::Pubkey;
use notedeck::ColorTheme;

use crate::cache::BoardCache;
use crate::event;
use crate::ui::{board_inline_ui, card_chip_ui, card_inline_ui, issue_inline_ui};

/// Renders a headway issue (kind 1621) referenced inline, e.g. from a notebook
/// note. Registered into [`notedeck::KindRendererRegistry`] at app startup.
///
/// The kind-1621 note is only the card's *creation-time* snapshot; its current
/// title/labels/description come from folding the owning board's later edits. So
/// we fold the board (cached, see [`BoardCache`]) and render the resolved
/// [`event::CardView`], falling back to the raw snapshot if the board isn't local.
pub struct HeadwayIssueRenderer {
    pub(crate) cache: Rc<RefCell<BoardCache>>,
}

impl notedeck::KindRenderer for HeadwayIssueRenderer {
    fn id(&self) -> &'static str {
        "headway.issue"
    }
    fn name(&self) -> &'static str {
        "Headway issue"
    }
    fn kinds(&self) -> &'static [u32] {
        &[event::KIND_ISSUE]
    }
    fn render(
        &self,
        ui: &mut egui::Ui,
        note_context: &mut notedeck::NoteContext,
        req: &notedeck::KindRenderRequest,
    ) -> notedeck::KindRenderResponse {
        profiling::scope!("HeadwayIssueRenderer::render");
        let note = req.note;
        let theme = ColorTheme::current(ui.ctx());
        let Some(event::HeadwayEvent::Issue(issue)) = event::parse(note) else {
            return notedeck::KindRenderResponse::new(ui.weak("invalid headway issue"));
        };
        let author = Pubkey::new(issue.board_author);
        // Resolve the card off the (cached) folded board and draw the shape the
        // context asks for: a compact chip inline in prose, the full card as a
        // block embed. Resolve across *all* the author's boards, not the card's
        // `a`-tag board: a cross-board move leaves the `a` tag on the origin board
        // while the card lives on the destination (see [`event::locate_card`]).
        // `.and_then` resolves owned data so the cache borrow drops before drawing.
        // Locate the card across *all* the author's boards off the memoized
        // finalize (see [`with_boards`]), not the card's `a`-tag board: a
        // cross-board move leaves the `a` tag on the origin while the card lives
        // on the destination (see [`event::locate_card`]). Resolve to owned data
        // so the cache borrow drops before drawing.
        let located = self
            .cache
            .borrow_mut()
            .with_boards(note_context.ndb, req.txn, &author, |boards| {
                event::locate_card_in_boards(boards, &author, &issue.id)
            })
            .flatten();
        let response = match req.context {
            notedeck::RenderContext::Inline => match located {
                Some(located) => card_chip_ui(ui, &theme, &located.card.title, located.column),
                // Card on no folded board: chip from the creation-time snapshot.
                None => card_chip_ui(ui, &theme, &issue.subject, None),
            },
            _ => match located {
                Some(located) => card_inline_ui(ui, &theme, &located.card),
                // Card on no folded board: show the creation-time snapshot.
                None => issue_inline_ui(ui, &theme, &issue),
            },
        };
        notedeck::open_on_click(ui, response, note)
    }
}

/// Renders a headway board (kind 30619) referenced inline. The note is the
/// addressable board event; we recover its `(author, board_id)` and fold the
/// full board (cached, see [`BoardCache`]) off the local db to summarise it.
pub struct HeadwayBoardRenderer {
    pub(crate) cache: Rc<RefCell<BoardCache>>,
}

impl notedeck::KindRenderer for HeadwayBoardRenderer {
    fn id(&self) -> &'static str {
        "headway.board"
    }
    fn name(&self) -> &'static str {
        "Headway board"
    }
    fn kinds(&self) -> &'static [u32] {
        &[event::KIND_BOARD]
    }
    fn render(
        &self,
        ui: &mut egui::Ui,
        note_context: &mut notedeck::NoteContext,
        req: &notedeck::KindRenderRequest,
    ) -> notedeck::KindRenderResponse {
        profiling::scope!("HeadwayBoardRenderer::render");
        let note = req.note;
        let theme = ColorTheme::current(ui.ctx());
        let Some(event::HeadwayEvent::Board(board)) = event::parse(note) else {
            return notedeck::KindRenderResponse::new(ui.weak("invalid headway board"));
        };
        let author = Pubkey::new(board.author);
        let view = self
            .cache
            .borrow_mut()
            .board(note_context.ndb, req.txn, &author, &board.id);
        let response = match view {
            Some(view) => board_inline_ui(ui, &theme, &view),
            None => ui.weak("headway board not found"),
        };
        notedeck::open_on_click(ui, response, note)
    }
}

/// The inline reference parser for a headway card id —
/// `headway:<board-slug>/<word>-<word>-<word>` (e.g.
/// `headway:commerce/purse-metal-toilet`) — the same canonical form a human or the
/// CLI writes. Registered via [`App::reference_parsers`](notedeck::App::reference_parsers); the browser's markdown
/// scanner recognises the ref in a run of text
/// ([`find`](notedeck::ReferenceParser::find)),
/// [`resolve`](notedeck::ReferenceParser::resolve)s it to the card's kind-1621
/// issue note, and draws it with the already-registered [`HeadwayIssueRenderer`].
///
/// The `headway:` scheme is **required** in free text: the scheme-less
/// `<board>/<word-id>` shorthand accepted at interactive resolver input (the CLI,
/// the board filter) is too ambiguous against ordinary `word/word` prose to scan
/// for here. [`find`] recognises the shape cheaply (scheme, slug, `/`, three
/// dash-joined words); [`resolve`] is the authority — a candidate that doesn't
/// fold to a real card is re-rendered as plain text, so a loose match is
/// invisible, not a broken chip. Unlike the old `#` sigil, `:` and `/` survive
/// nostrdb's tokenizer, so the ref stays whole inside note content.
///
/// **Author gap.** A headway ref carries no pubkey, but folding a board needs
/// one. We resolve against [`ReferenceResolveCtx::selected_account`], so a ref
/// only resolves to a board the *current* account owns. A later grammar
/// extension can carry an explicit author (e.g. an `naddr`-style coordinate).
pub struct HeadwayRefParser {
    /// Shared with the app's kind renderers (see [`Headway::board_cache`](crate::Headway::board_cache)), so a
    /// card referenced by word id folds the same cached board a directly-embedded
    /// card/board does — the fold happens once per board, not once per surface.
    pub(crate) cache: Rc<RefCell<BoardCache>>,
}

impl HeadwayRefParser {
    /// A byte is part of a board slug: lowercase ascii, a digit, or `-` (see
    /// `store::board_slug`). Uppercase is intentionally excluded — slugs are
    /// lowercase — but `resolve` lowercases anyway, so a stray capital only costs
    /// a slug boundary, never a wrong resolution.
    fn is_slug_byte(b: u8) -> bool {
        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
    }
}

impl notedeck::ReferenceParser for HeadwayRefParser {
    fn id(&self) -> &'static str {
        "headway"
    }

    #[profiling::function]
    fn find(&self, text: &str) -> Option<std::ops::Range<usize>> {
        const SCHEME: &str = "headway:";
        let bytes = text.as_bytes();
        let mut search = 0;
        while let Some(rel) = text[search..].find(SCHEME) {
            let start = search + rel;
            // Continue past this scheme on the next iteration regardless of outcome.
            search = start + SCHEME.len();

            // The scheme must begin at a slug boundary, so `myheadway:` isn't a
            // match latched onto the tail of a longer word.
            if start > 0 && Self::is_slug_byte(bytes[start - 1]) {
                continue;
            }

            // Board slug: a run of slug bytes after the scheme, then a single `/`.
            let slug_start = search;
            let mut i = slug_start;
            while i < bytes.len() && Self::is_slug_byte(bytes[i]) {
                i += 1;
            }
            if i == slug_start || i >= bytes.len() || bytes[i] != b'/' {
                continue;
            }

            // Three dash-joined words right after the `/` (the shared word-id
            // grammar; see [`headway::wordid::three_words_end`]).
            if let Some(end) = headway::wordid::three_words_end(bytes, i + 1) {
                return Some(start..end);
            }
        }
        None
    }

    #[profiling::function]
    fn resolve(
        &self,
        matched: &str,
        ctx: &notedeck::ReferenceResolveCtx,
    ) -> Option<notedeck::ResolvedRef> {
        let (slug, words) = headway::wordid::parse_ref(matched)?;
        // Author gap: no pubkey in the ref, so fold the current account's boards.
        let author = ctx.selected_account?;
        let board_id = slug.to_lowercase();

        // Fold (memoized) the board, then re-encode its cards to match the word id.
        // The closure yields the owned `NoteId` so the cache borrow drops here, and
        // resolving through the shared finalize means a chip drawn right after
        // reuses this frame's fold instead of walking the reducer again.
        let note_id = self
            .cache
            .borrow_mut()
            .with_boards(ctx.ndb, ctx.txn, &author, |boards| {
                event::find_board(boards, &author, &board_id)
                    .and_then(|board| event::resolve_card_by_wordid(board, words))
            })
            .flatten()?;
        Some(notedeck::ResolvedRef::note(note_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostrdb::Transaction;

    use crate::cache::tests::TestSync;
    use crate::store;

    fn ref_parser() -> HeadwayRefParser {
        HeadwayRefParser {
            cache: Rc::new(RefCell::new(BoardCache::default())),
        }
    }

    /// [`HeadwayRefParser::find`] is a purely syntactic filter: it recognises the
    /// scheme-prefixed `headway:<slug>/<word-word-word>` shape and rejects
    /// near-misses — a bare `slug/word-id` with no scheme, non-three-word runs —
    /// leaving the rest to `resolve`.
    #[test]
    fn ref_parser_find_recognises_scheme_refs() {
        use notedeck::ReferenceParser;
        let p = ref_parser();
        let hit = |s: &str| p.find(s).map(|r| s[r].to_string());

        // A canonical ref, standalone and embedded in prose.
        assert_eq!(
            hit("headway:commerce/purse-metal-toilet").as_deref(),
            Some("headway:commerce/purse-metal-toilet")
        );
        assert_eq!(
            hit("see headway:commerce/purse-metal-toilet here").as_deref(),
            Some("headway:commerce/purse-metal-toilet")
        );
        // Slugs may carry digits and dashes (see `store::board_slug`).
        assert_eq!(
            hit("headway:2024-goals/maple-river-canyon").as_deref(),
            Some("headway:2024-goals/maple-river-canyon")
        );
        // A trailing period (or any non-letter) ends the third word.
        assert_eq!(
            hit("done: headway:commerce/purse-metal-toilet.").as_deref(),
            Some("headway:commerce/purse-metal-toilet")
        );

        // The `headway:` scheme is required — a bare `slug/word-id` is not matched.
        assert!(hit("commerce/purse-metal-toilet").is_none());
        // The scheme must sit on a word boundary, not the tail of a longer word.
        assert!(hit("myheadway:commerce/purse-metal-toilet").is_none());
        // A scheme with no slug, or no `/`, is not a ref.
        assert!(hit("headway:/purse-metal-toilet").is_none());
        assert!(hit("headway:commerce-purse-metal-toilet").is_none());
        // Fewer or more than three words is not a word id.
        assert!(hit("headway:board/one-two").is_none());
        assert!(hit("headway:board/one-two-three-four").is_none());
        // Uppercase words aren't BIP-39 words.
        assert!(hit("headway:board/One-Two-Three").is_none());
        // Plain prose yields nothing.
        assert!(hit("just a sentence, nothing here").is_none());

        // A rejected candidate doesn't swallow a later valid ref.
        assert_eq!(
            hit("headway:board/one-two then headway:b/red-green-blue").as_deref(),
            Some("headway:b/red-green-blue")
        );
    }

    /// [`HeadwayRefParser::resolve`] folds the referenced board (via the shared
    /// [`BoardCache`]) and re-encodes its cards to match the word id,
    /// yielding the card's kind-1621 issue note. It resolves relative to the
    /// selected account (the author gap), so no account means no resolution.
    #[tokio::test]
    async fn ref_parser_resolves_word_id_to_card() {
        use notedeck::{ReferenceParser, ReferenceResolveCtx};
        let mut t = TestSync::new();
        t.poll();
        t.seed_and_settle().await;

        // Take a real card and its word id off the folded board.
        let (card_id, words) = {
            let txn = Transaction::new(&t.ndb).unwrap();
            let board = event::load_board(&t.ndb, &txn, &t.kp.pubkey, store::BOARD_ID).unwrap();
            let card = board.columns.iter().flat_map(|c| &c.cards).next().unwrap();
            (card.id, headway::wordid::encode(card.id.bytes()))
        };
        let matched = format!("headway:{}/{}", store::BOARD_ID, words);

        let p = ref_parser();
        // In the app the parser shares the board cache `update` already drives
        // (see `reference_parsers`), so it reads an already-seeded one. This
        // parser owns a fresh cache, so drive the two advances that seed it —
        // the first subscribes, the next folds the history (see
        // `RealtimeCache::advance`) — before resolving against it.
        for _ in 0..2 {
            let txn = Transaction::new(&t.ndb).unwrap();
            p.cache.borrow_mut().poll(&t.ndb, &txn, &t.kp.pubkey);
        }

        let txn = Transaction::new(&t.ndb).unwrap();
        let ctx = ReferenceResolveCtx {
            ndb: &t.ndb,
            txn: &txn,
            selected_account: Some(t.kp.pubkey),
        };
        // The canonical ref resolves to that card's issue note.
        assert_eq!(p.resolve(&matched, &ctx).unwrap().note_id, card_id);
        // A well-formed word id that encodes no card doesn't resolve.
        let bogus = format!("headway:{}/zoo-zoo-zoo", store::BOARD_ID);
        assert!(p.resolve(&bogus, &ctx).is_none());
        // Without a selected account the author gap can't be filled.
        let ctx_no_acct = ReferenceResolveCtx {
            ndb: &t.ndb,
            txn: &txn,
            selected_account: None,
        };
        assert!(p.resolve(&matched, &ctx_no_acct).is_none());
    }
}
