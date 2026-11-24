//! Inline renderers — drawing a single headway entity referenced from elsewhere
//! (e.g. a `nostr:` link in a notebook note), via notedeck's `KindRenderer`
//! registry, plus the `headway:<board>/<word-id>` [`ReferenceParser`](notedeck::ReferenceParser).
//! These are read-only and self-contained, unlike the editable board.

use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;

use nostrdb::{Ndb, Note, Transaction};
use notedeck::ColorTheme;

use crate::cache::{BoardCache, LocatedIssue};
use crate::event;
use crate::ui::{ChipStatus, board_inline_ui, card_chip_ui, card_inline_ui, issue_inline_ui};

/// Renders a headway issue (kind 1621) referenced inline, e.g. from a notebook
/// note. Registered into [`notedeck::KindRendererRegistry`] at app startup.
///
/// The kind-1621 note is only the card's *creation-time* snapshot; its current
/// title/labels/description come from folding the board it lives on. So we
/// locate the card by coordinate ([`BoardCache::locate_issue`]: every roster
/// board's multi-writer fold, so a card on a board shared with us, or one a
/// teammate added to ours, resolves) and render the folded card, falling back
/// to the raw snapshot if no local board holds it.
///
/// The roster is the selected account's: [`render`](notedeck::KindRenderer::render)
/// reads it for that account, while [`title`](notedeck::KindRenderer::title),
/// [`outline`](notedeck::KindRenderer::outline) and
/// [`revision`](notedeck::KindRenderer::revision), handed no account, read
/// whichever roster the cache holds.
pub struct HeadwayIssueRenderer {
    pub(crate) cache: Rc<RefCell<BoardCache>>,
}

impl HeadwayIssueRenderer {
    /// Locate the card `issue` stands for on the board it lives on (see
    /// [`BoardCache::locate_issue`]), on `me`'s roster when given. A handle
    /// into the memoized fold, so the cache borrow drops before drawing and the
    /// card is borrowed out of it rather than copied.
    fn locate(
        &self,
        ndb: &Ndb,
        txn: &Transaction,
        me: Option<&nostrdb_net::Pubkey>,
        issue: &event::IssueEvent,
    ) -> Option<LocatedIssue> {
        let origin = event::IssueBoard {
            owner: issue.board_author,
            slug: &issue.board_id,
        };
        self.cache
            .borrow_mut()
            .locate_issue(ndb, txn, me, &origin, &issue.id)
    }
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
        // Resolve the card where it lives and draw the shape the context asks
        // for: a compact chip inline in prose, the full card as a block embed.
        let me = note_context.accounts.selected_account_pubkey();
        let located = self.locate(note_context.ndb, req.txn, Some(me), &issue);
        let found = located.as_ref().and_then(LocatedIssue::card);
        let response = match req.context {
            // A list row is the chip too: the card's live status and title.
            notedeck::RenderContext::Inline | notedeck::RenderContext::Row => match found {
                Some(found) => card_chip_ui(
                    ui,
                    &theme,
                    &found.card.title,
                    ChipStatus::located(found.column),
                    req.chrome,
                ),
                // Card on no folded board: chip from the creation-time snapshot.
                None => card_chip_ui(ui, &theme, &issue.subject, ChipStatus::Unplaced, req.chrome),
            },
            _ => match found {
                Some(found) => card_inline_ui(ui, &theme, found.card),
                // Card on no folded board: show the creation-time snapshot.
                None => issue_inline_ui(ui, &theme, &issue),
            },
        };
        notedeck::open_on_click(ui, response, req)
    }

    /// The card's current title, folded off its board the same way
    /// [`render`](Self::render) finds it, falling back to the creation-time
    /// subject when the card is on no folded board.
    fn title(&self, ndb: &Ndb, txn: &Transaction, note: &Note) -> Option<String> {
        let Some(event::HeadwayEvent::Issue(issue)) = event::parse(note) else {
            return None;
        };
        let located = self.locate(ndb, txn, None, &issue);
        let title = located
            .as_ref()
            .and_then(LocatedIssue::card)
            .map_or(issue.subject.as_str(), |found| found.card.title.as_str());
        Some(title.to_owned())
    }

    /// The card's parent card, as a `headway:<board>/<word-id>` ref on the
    /// board the card was found on, and how many of its live subissues are
    /// done. `None` when the card is on no folded board.
    fn outline(&self, ndb: &Ndb, txn: &Transaction, note: &Note) -> Option<notedeck::Outline> {
        let Some(event::HeadwayEvent::Issue(issue)) = event::parse(note) else {
            return None;
        };
        let located = self.locate(ndb, txn, None, &issue)?;
        let card = located.card()?.card;
        let parent = card
            .parent
            .map(|parent| headway::wordid::card_ref(&located.board().id, parent.bytes()));
        let live = || card.subissues.iter().filter(|s| !s.archived);
        let total = live().count();
        let parts = (total > 0).then(|| notedeck::Progress {
            done: live().filter(|s| s.done).count(),
            total,
        });
        Some(notedeck::Outline { parent, parts })
    }

    /// The folds [`title`](Self::title) and [`outline`](Self::outline) read
    /// (see [`BoardCache::ref_revision`]): every board, since the card may have
    /// moved to any of them. Any edit to any of them moves it.
    fn revision(&self, _ndb: &Ndb, _txn: &Transaction, note: &Note) -> Option<u64> {
        // Asked for every card-titled row every frame, so it only checks the
        // note is a card, off the `a` tag without parsing (and copying) it.
        event::issue_board(note)?;
        Some(self.cache.borrow().ref_revision())
    }
}

/// Renders a headway board (kind 30619) referenced inline. The note is the
/// addressable board event; we recover its coordinate and fold the full board
/// by it (cached, see [`BoardCache::board_at`]) off the local db to summarise
/// it: a shared board through its multi-writer fold, so every member's cards
/// count.
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
        let view =
            self.cache
                .borrow_mut()
                .board_at(note_context.ndb, req.txn, &board.author, &board.id);
        let response = match view {
            Some(view) => board_inline_ui(ui, &theme, &view),
            None => ui.weak("headway board not found"),
        };
        notedeck::open_on_click(ui, response, req)
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
/// **Owner gap.** A headway ref carries a slug but no owner, and a board is a
/// coordinate (owner + slug). We resolve it relative to
/// [`ReferenceResolveCtx::selected_account`]: the account's own board at that
/// slug first, then a board someone shared with it at that slug (see
/// [`BoardCache::resolve_ref`]), each folded by coordinate so a teammate's
/// card resolves too. A later grammar extension can carry an explicit owner
/// (e.g. an `naddr`-style coordinate).
pub struct HeadwayRefParser {
    /// Shared with the app's kind renderers (see [`Headway::board_cache`](crate::Headway::board_cache)), so a
    /// card referenced by word id folds the same cached board a directly-embedded
    /// card/board does — the fold happens once per board, not once per surface.
    pub(crate) cache: Rc<RefCell<BoardCache>>,
}

impl notedeck::ReferenceParser for HeadwayRefParser {
    fn id(&self) -> &'static str {
        "headway"
    }

    /// The shared grammar: see [`headway::wordid::find_ref`].
    #[profiling::function]
    fn find(&self, text: &str) -> Option<std::ops::Range<usize>> {
        headway::wordid::find_ref(text)
    }

    #[profiling::function]
    fn resolve(
        &self,
        matched: &str,
        ctx: &notedeck::ReferenceResolveCtx,
    ) -> Option<notedeck::ResolvedRef> {
        let headway::wordid::CardRef { board: slug, words } = headway::wordid::parse_ref(matched)?;
        // Owner gap: no pubkey in the ref, so resolve relative to the account.
        let me = ctx.selected_account?;
        // Resolving through the shared finalize means a chip drawn right after
        // reuses this frame's fold instead of walking the reducer again.
        let note_id =
            self.cache
                .borrow_mut()
                .resolve_ref(ctx.ndb, ctx.txn, &me, &board_slug(slug), words)?;
        Some(notedeck::ResolvedRef::note(note_id))
    }

    /// The folds [`resolve`](Self::resolve) reads (see
    /// [`BoardCache::ref_revision`]): every roster board. A card arriving, or
    /// any other edit to those, moves it.
    fn revision(&self, matched: &str, ctx: &notedeck::ReferenceResolveCtx) -> Option<u64> {
        headway::wordid::parse_ref(matched)?;
        ctx.selected_account?;
        Some(self.cache.borrow().ref_revision())
    }
}

/// A ref's board slug as boards store it: lowercase. Borrowed when it already
/// is, which a ref written by headway always is, so the per-frame
/// [`revision`](HeadwayRefParser::revision) check copies nothing.
fn board_slug(slug: &str) -> Cow<'_, str> {
    if slug.chars().any(char::is_uppercase) {
        Cow::Owned(slug.to_lowercase())
    } else {
        Cow::Borrowed(slug)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostrdb::Transaction;

    use crate::cache::tests::TestSync;
    use crate::store;

    /// Pump `cache` for `t`'s account until it settles, as the app's `update`
    /// does every frame: the first pass puts its boards on the roster and
    /// subscribes them, the next seeds them.
    fn pump(cache: &RefCell<BoardCache>, t: &TestSync) {
        let mut cache = cache.borrow_mut();
        cache.set_roster(&t.ndb, &t.kp.pubkey, &[]);
        for _ in 0..3 {
            let txn = Transaction::new(&t.ndb).unwrap();
            cache.poll(&t.ndb, &txn);
        }
    }

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
        // parser owns a fresh cache, so pump it as `update` would.
        pump(&p.cache, &t);

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

    /// [`KindRendererRegistry::title`](notedeck::KindRendererRegistry::title)
    /// reaches [`HeadwayIssueRenderer::title`] for a card's kind-1621 note and
    /// returns the card's *current* title, folded off its board, not the
    /// creation-time subject the note itself carries.
    #[tokio::test]
    async fn registry_title_is_the_folded_card_title() {
        let mut t = TestSync::new();
        t.poll();
        t.seed_and_settle().await;

        let cache = Rc::new(RefCell::new(BoardCache::default()));
        let mut registry = notedeck::KindRendererRegistry::default();
        registry.register(Box::new(HeadwayIssueRenderer {
            cache: cache.clone(),
        }));

        // Rename a real card, so its folded title differs from its note's subject.
        let card_id = {
            let view = t.view().expect("board loaded");
            let card_id = view.columns[0].cards[0].id;
            store::apply(
                &t.ndb,
                store::BOARD_ID,
                &view,
                &t.kp.pubkey,
                &store::Signer::new(&t.kp.secret_key.secret_bytes(), None),
                store::BoardAction::EditTitle {
                    card: card_id,
                    title: "UT7c: renamed — after creation".to_string(),
                },
                &mut store::NoPublish,
            );
            card_id
        };
        t.wait(|v| {
            v.columns[0]
                .cards
                .iter()
                .any(|c| c.title == "UT7c: renamed — after creation")
        })
        .await;

        // Seed the registry's own cache, as `update` would.
        pump(&cache, &t);

        let txn = Transaction::new(&t.ndb).unwrap();
        let note = t.ndb.get_note_by_id(&txn, card_id.bytes()).unwrap();
        assert_ne!(
            event::parse(&note).and_then(|e| match e {
                event::HeadwayEvent::Issue(issue) => Some(issue.subject),
                _ => None,
            }),
            Some("UT7c: renamed — after creation".to_string())
        );
        assert_eq!(
            registry.title(&t.ndb, &txn, &note, None).as_deref(),
            Some("UT7c: renamed — after creation")
        );
    }

    /// A card ref looked up through the registries is versioned by the
    /// parser and the renderer together: the revision holds while the board
    /// is idle, so a caller keeping the title doesn't look again, and moves
    /// once the card is renamed, when the next lookup finds the new title.
    #[tokio::test]
    async fn a_card_refs_revision_holds_until_the_card_is_renamed() {
        /// One lookup of the card ref, and the revision a check right after
        /// it reports.
        struct Looked {
            title: Option<String>,
            kept: Option<notedeck::ReferenceRevision>,
            current: Option<notedeck::ReferenceRevision>,
        }

        let mut t = TestSync::new();
        t.poll();
        t.seed_and_settle().await;

        let cache = Rc::new(RefCell::new(BoardCache::default()));
        let mut registries = notedeck::AppRegistries::default();
        registries
            .reference_parsers
            .register(Box::new(HeadwayRefParser {
                cache: cache.clone(),
            }));
        registries
            .kind_renderers
            .register(Box::new(HeadwayIssueRenderer {
                cache: cache.clone(),
            }));
        pump(&cache, &t);

        let (card_id, card_ref) = {
            let view = t.view().expect("board loaded");
            let card = &view.columns[0].cards[0];
            (
                card.id,
                headway::wordid::card_ref(&view.id, card.id.bytes()),
            )
        };
        let ndb = t.ndb.clone();
        let author = t.kp.pubkey;
        let lookup = |registries: &notedeck::AppRegistries| {
            let txn = Transaction::new(&ndb).unwrap();
            let ctx = notedeck::ReferenceResolveCtx {
                ndb: &ndb,
                txn: &txn,
                selected_account: Some(author),
            };
            let found = registries.lookup_reference(&card_ref, &ctx, |note| {
                registries.kind_renderers.title(&ndb, &txn, note, None)
            });
            Looked {
                title: found.value.flatten(),
                kept: found.revision,
                current: registries.reference_revision(&card_ref, &ctx, found.note),
            }
        };

        let first = lookup(&registries);
        assert!(first.title.is_some());
        let kept = first.kept.expect("a headway card ref is versioned");
        assert_eq!(
            first.current,
            Some(kept),
            "idle: the kept lookup is current"
        );

        // Subscribed before the edit, so the wait below can't miss its ingest.
        let mut stream = crate::cache::tests::ingest_stream(&t.ndb, &t.kp.pubkey);
        {
            let view = t.view().expect("board loaded");
            store::apply(
                &t.ndb,
                store::BOARD_ID,
                &view,
                &t.kp.pubkey,
                &store::Signer::new(&t.kp.secret_key.secret_bytes(), None),
                store::BoardAction::EditTitle {
                    card: card_id,
                    title: "Renamed on its board".to_string(),
                },
                &mut store::NoPublish,
            );
        }
        // The registries' own cache folds the rename on its next pump (the
        // app's `update`), which moves the revision.
        loop {
            pump(&cache, &t);
            if lookup(&registries).current != Some(kept) {
                break;
            }
            notedeck_testing::await_batch(&mut stream).await;
        }
        assert_eq!(
            lookup(&registries).title.as_deref(),
            Some("Renamed on its board")
        );
    }

    /// A sealed board at `slug`, owned by `owner`, on one SNS channel that the
    /// renderers' cache has on its roster, as a key-share would put it there.
    struct SharedBoard {
        cache: Rc<RefCell<BoardCache>>,
        channel: store::SnsChannel,
        slug: &'static str,
        owner: nostrdb_net::FullKeypair,
    }

    impl SharedBoard {
        /// Register a fresh channel root, put the board on the cache's roster,
        /// subscribe its envelope leg, then seal the board's definition, signed
        /// by `owner`, into the channel and wait for it to fold.
        async fn new(t: &TestSync, owner: nostrdb_net::FullKeypair, slug: &'static str) -> Self {
            // A root of the owner's own, so two boards' channels differ.
            let mut root = *owner.pubkey.bytes();
            root[0] ^= 0x5a;
            assert!(t.ndb.add_team_root(&root));
            let channel = store::SnsChannel {
                keys: nostrdb_net::sns::derive_sns_keys(&root).expect("keys"),
            };
            let teams = vec![crate::teams::Team {
                team_root: hex::encode(root),
                board_addr: event::board_address(&owner.pubkey, slug),
                epoch: None,
                shared_at: 1,
            }];
            let cache = Rc::new(RefCell::new(BoardCache::default()));
            // The reader's roster: `t`'s account holds the key-share.
            cache.borrow_mut().set_roster(&t.ndb, &t.kp.pubkey, &teams);
            let board = Self {
                cache,
                channel,
                slug,
                owner,
            };
            // Subscribe the envelope leg before anything is sealed, so every
            // later envelope is reported and re-folds the board.
            board.poll(t);

            let mut stream = crate::cache::tests::ingest_stream(&t.ndb, &board.owner.pubkey);
            let cols = vec![
                event::ColumnDef::new("backlog", "Backlog"),
                event::ColumnDef::new("done", "Done"),
            ];
            store::ingest_signed(
                &t.ndb,
                event::build_board(slug, "Shared", "", &cols),
                &store::Signer::shared(&board.owner.secret_key.secret_bytes(), &board.channel),
                &mut store::NoPublish,
            );
            while board.view(t).is_none() {
                notedeck_testing::await_batch(&mut stream).await;
            }
            board
        }

        /// One pass of the app's per-frame shared pump (`Headway::update`).
        fn poll(&self, t: &TestSync) {
            let txn = Transaction::new(&t.ndb).unwrap();
            self.cache.borrow_mut().poll(&t.ndb, &txn);
        }

        /// The board's multi-writer fold, if its definition has folded in.
        fn view(&self, t: &TestSync) -> Option<crate::cache::BoardRef> {
            self.poll(t);
            let txn = Transaction::new(&t.ndb).unwrap();
            self.cache
                .borrow_mut()
                .board_at(&t.ndb, &txn, self.owner.pubkey.bytes(), self.slug)
                .filter(|v| !v.columns.is_empty())
        }

        /// Seal a card titled `title`, signed by `member`, onto the board and
        /// wait for its fold to show it. Returns the card's id.
        async fn add_card(
            &self,
            t: &TestSync,
            member: &nostrdb_net::FullKeypair,
            title: &str,
        ) -> nostrdb_net::NoteId {
            let mut stream = crate::cache::tests::ingest_stream(&t.ndb, &member.pubkey);
            let view = self.view(t).expect("board folded");
            let card = store::apply_outcome(
                &t.ndb,
                self.slug,
                &view,
                &member.pubkey,
                &store::Signer::new(&member.secret_key.secret_bytes(), Some(&self.channel)),
                store::BoardAction::AddCard(store::NewCard {
                    col: 0,
                    title: title.to_string(),
                    ..store::NewCard::default()
                }),
                &mut store::NoPublish,
            )
            .created
            .expect("card created");
            while !self
                .view(t)
                .is_some_and(|v| event::card_in_board(&v, card.bytes()).is_some())
            {
                notedeck_testing::await_batch(&mut stream).await;
            }
            card
        }

        /// The ref parser and issue renderer, registered over this board's cache.
        fn registries(&self) -> notedeck::AppRegistries {
            let mut registries = notedeck::AppRegistries::default();
            registries
                .reference_parsers
                .register(Box::new(HeadwayRefParser {
                    cache: self.cache.clone(),
                }));
            registries
                .kind_renderers
                .register(Box::new(HeadwayIssueRenderer {
                    cache: self.cache.clone(),
                }));
            registries
        }
    }

    /// Resolve `card_ref` as `me` through `registries`, returning the card's
    /// id and its title as the issue renderer folds it.
    fn lookup(
        t: &TestSync,
        registries: &notedeck::AppRegistries,
        me: nostrdb_net::Pubkey,
        card_ref: &str,
    ) -> Option<ChipLookup> {
        let txn = Transaction::new(&t.ndb).unwrap();
        let ctx = notedeck::ReferenceResolveCtx {
            ndb: &t.ndb,
            txn: &txn,
            selected_account: Some(me),
        };
        let found = registries.lookup_reference(card_ref, &ctx, |note| {
            registries.kind_renderers.title(&t.ndb, &txn, note, None)
        });
        let note = t.ndb.get_note_by_key(&txn, found.note?).ok()?;
        Some(ChipLookup {
            id: nostrdb_net::NoteId::new(*note.id()),
            title: found.value.flatten(),
        })
    }

    /// What [`lookup`] found for a ref: the card's note and its folded title.
    struct ChipLookup {
        id: nostrdb_net::NoteId,
        title: Option<String>,
    }

    /// A ref to a card on a board another key owns and shared with us
    /// resolves, by the board's coordinate, to the card and its title: the
    /// agentium/Dave chip that stayed plain text while chips read only the
    /// selected account's own author fold.
    #[tokio::test]
    async fn ref_resolves_a_card_on_a_board_shared_with_us() {
        let t = TestSync::new();
        let owner = nostrdb_net::FullKeypair::generate();
        let board = SharedBoard::new(&t, owner, "team").await;
        let card = board.add_card(&t, &board.owner, "Owner's card").await;

        let card_ref = headway::wordid::card_ref(board.slug, card.bytes());
        let found = lookup(&t, &board.registries(), t.kp.pubkey, &card_ref)
            .expect("a joined board's card resolves");
        assert_eq!(found.id, card);
        assert_eq!(found.title.as_deref(), Some("Owner's card"));
    }

    /// A ref to a card a member added to our own sealed board resolves: the
    /// card is the member's, so it is in no author fold of ours, only in the
    /// board's multi-writer fold.
    #[tokio::test]
    async fn ref_resolves_a_card_a_member_added_to_our_board() {
        let t = TestSync::new();
        let board = SharedBoard::new(&t, t.kp.clone(), "ours").await;
        let member = nostrdb_net::FullKeypair::generate();
        let card = board.add_card(&t, &member, "Member's card").await;

        let card_ref = headway::wordid::card_ref(board.slug, card.bytes());
        let found = lookup(&t, &board.registries(), t.kp.pubkey, &card_ref)
            .expect("a member's card on our board resolves");
        assert_eq!(found.id, card);
        assert_eq!(found.title.as_deref(), Some("Member's card"));

        // A kept chip looks again once the shared fold moves: another
        // member's edit re-folds the board and moves the ref's revision.
        let registries = board.registries();
        let revision = || {
            let txn = Transaction::new(&t.ndb).unwrap();
            let ctx = notedeck::ReferenceResolveCtx {
                ndb: &t.ndb,
                txn: &txn,
                selected_account: Some(t.kp.pubkey),
            };
            registries.reference_parsers.revision_exact(&card_ref, &ctx)
        };
        let before = revision().expect("a headway ref is versioned");
        assert_eq!(revision(), Some(before), "idle: the revision holds");
        board.add_card(&t, &member, "Another card").await;
        assert_ne!(revision(), Some(before), "a shared edit moves it");
    }

    /// A slug no board of ours or shared with us has resolves to nothing,
    /// and so stays plain text.
    #[tokio::test]
    async fn ref_to_an_unknown_slug_does_not_resolve() {
        let t = TestSync::new();
        let owner = nostrdb_net::FullKeypair::generate();
        let board = SharedBoard::new(&t, owner, "team").await;
        let card = board.add_card(&t, &board.owner, "Owner's card").await;

        let card_ref = headway::wordid::card_ref("elsewhere", card.bytes());
        assert!(lookup(&t, &board.registries(), t.kp.pubkey, &card_ref).is_none());
    }
}
