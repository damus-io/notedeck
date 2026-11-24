//! The board-data engine: [`BoardCache`], one multi-writer fold per board
//! *coordinate* (`30619:<owner>:<slug>`), whoever owns the board and however it
//! is stored. The foreground board, the switcher and every inline widget read
//! it.
//!
//! An account's own board is a team-of-one shared board: it folds through the
//! same [`event::SharedFold`] as a board someone shared with us, over the
//! channels its key-shares name. A plaintext board, created before boards were
//! born sealed, has no channel and folds from its owner's signed plaintext
//! alone (see `event::shared_fold_admits`).

use std::collections::HashMap;
use std::ops::Deref;
use std::rc::Rc;

use nostrdb::{Ndb, NoteKey, Subscription, Transaction};
use nostrdb_net::{NoteId, Pubkey};

use crate::event::{self, BoardView};
use crate::teams;

/// The single board-data engine for the Headway app, shared behind
/// `Rc<RefCell<…>>` between the app and everything it registers for inline
/// display.
///
/// The foreground board and every inline widget — the
/// [`KindRenderer`](notedeck::KindRenderer)s and the
/// [`ReferenceParser`](notedeck::ReferenceParser) — read the *same* folds, so a
/// realtime edit (a CLI move, a remote sync) that [`update`](notedeck::App::update)'s
/// per-frame [`poll`](Self::poll) folds in is immediately visible to an inline
/// chip drawn in another app, not just to the open board.
///
/// Every board folds by coordinate ([`folds`](Self::folds)), so there is one
/// fold of a board however many surfaces read it, and a teammate's card on a
/// board we own is in the same fold that draws it.
#[derive(Default)]
pub(crate) struct BoardCache {
    /// Every board folded so far, keyed by board coordinate
    /// (`30619:owner:slug`). Each folds through an [`event::SharedFold`] so every
    /// member's events are gathered: seeded once by walking the board's
    /// history, then fed what its subscription reports as deltas.
    folds: HashMap<String, BoardFold>,
    /// The last revision handed to a fold (see [`BoardFold::revision`]): one
    /// counter for every board. Also moved when the [`roster`](Self::roster)
    /// changes, so it moves whenever any lookup by coordinate could answer
    /// differently (see [`ref_revision`](Self::ref_revision)).
    revisions: u64,
    /// The account whose boards the [`roster`](Self::roster) lists.
    account: Option<Account>,
    /// Every board the account can open, one entry per coordinate: each board
    /// a key-share names (its own sealed boards and the ones shared with it),
    /// newest key-share first, then its plaintext boards. What the switcher
    /// lists, and what the inline renderers and the `headway:` ref parser
    /// resolve a board through, with each board's channels precomputed rather
    /// than re-derived per chip.
    roster: Vec<RosterBoard>,
}

/// The account a [`BoardCache`] lists boards for, and the two sources its
/// [`roster`](BoardCache::roster) is built from.
struct Account {
    /// The account's pubkey.
    me: Pubkey,
    /// One entry per board coordinate its key-shares name, newest first.
    keyshared: Vec<RosterBoard>,
    /// The slugs of its plaintext boards (see [`event::plaintext_board_slugs`]).
    plaintext: Vec<String>,
    /// Subscription over the account's board definitions: a new one may be a
    /// plaintext board to list, so it makes [`plaintext`](Self::plaintext)
    /// [`stale`](Self::stale).
    definitions: Option<Subscription>,
    /// [`plaintext`](Self::plaintext) has to be read again before the roster
    /// is next used.
    stale: bool,
}

/// One board on the account's roster (see [`BoardCache::roster`]): its
/// coordinate and every channel its multi-writer fold gathers.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RosterBoard {
    /// The board coordinate, `30619:<owner>:<slug>`: the key of its fold in
    /// [`BoardCache::folds`].
    addr: String,
    /// The board owner's pubkey.
    owner: [u8; 32],
    /// The board's slug.
    slug: String,
    /// The board's channel pubkeys, primary first (see
    /// [`teams::board_channel_pubkeys`]). Empty for a plaintext board.
    channels: Vec<Pubkey>,
    /// `created_at` of the newest key-share naming this board: which of two
    /// joined boards sharing a slug a bare ref means (see
    /// [`BoardCache::resolve_ref`]). Zero for a plaintext board.
    shared_at: u64,
}

impl RosterBoard {
    /// Is this the board at `owner`/`slug`?
    fn is(&self, owner: &[u8; 32], slug: &str) -> bool {
        &self.owner == owner && self.slug == slug
    }
}

/// One entry per board coordinate the key-shares in `teams` name, with the
/// channels its fold gathers, newest key-share first.
fn keyshared_boards(teams: &[teams::Team]) -> Vec<RosterBoard> {
    let mut boards: Vec<RosterBoard> = Vec::new();
    for team in teams {
        if let Some(known) = boards.iter_mut().find(|b| b.addr == team.board_addr) {
            known.shared_at = known.shared_at.max(team.shared_at);
            continue;
        }
        let Some(coord) = event::BoardCoord::parse(&team.board_addr) else {
            continue;
        };
        let channels = teams::board_channel_pubkeys(teams, &team.board_addr);
        if channels.is_empty() {
            continue;
        }
        boards.push(RosterBoard {
            addr: team.board_addr.clone(),
            owner: coord.owner,
            slug: coord.slug,
            channels,
            shared_at: team.shared_at,
        });
    }
    boards.sort_by_key(|b| std::cmp::Reverse(b.shared_at));
    boards
}

/// A card's issue note located on the board it lives on (see
/// [`BoardCache::locate_issue`]): a handle to that folded board, from which
/// [`card`](Self::card) borrows the card without copying it.
pub(crate) struct LocatedIssue {
    board: BoardRef,
    issue_id: [u8; 32],
}

impl LocatedIssue {
    /// The board the card lives on.
    pub(crate) fn board(&self) -> &BoardView {
        &self.board
    }

    /// The card and its live column, borrowed out of [`board`](Self::board).
    pub(crate) fn card(&self) -> Option<event::ResolvedCard<'_>> {
        event::card_with_column_in_board(&self.board, &self.issue_id)
    }
}

/// One board within [`BoardCache`]: its multi-writer fold by coordinate, plus
/// the subscription that feeds it deltas.
#[derive(Default)]
struct BoardFold {
    /// The board's fold, `None` until seeded (and again whenever it has to be
    /// re-seeded: its channels changed, or its subscription was just opened).
    fold: Option<event::SharedFold>,
    /// Memoized finalize of `fold`, rebuilt lazily after a re-fold. Shared so
    /// [`BoardRef`] can hand it out without copying.
    finalized: Option<Rc<[BoardView]>>,
    /// Which fold `fold` is at: moved by [`changed`](Self::changed) on every
    /// seed and every delta that folded something.
    revision: u64,
    /// Subscription over [`event::SharedFold::subscription_filters`]: the
    /// channels' envelopes and every note the board could come to hold, which
    /// [`event::SharedFold::apply`] sorts into this board's or not.
    sub: Option<Subscription>,
    /// The channels [`sub`](Self::sub) was opened over. When the roster's
    /// channels for the board stop matching, the subscription is replaced.
    sub_channels: Vec<Pubkey>,
    /// Keys the subscription reported that the last pass's snapshot couldn't
    /// read yet, retried on the next pass (see [`event::Applied::deferred`]).
    pending: Vec<NoteKey>,
    /// How many times this board's fold was seeded by walking its history
    /// here, the cost the delta path exists to avoid. Reseeds inside
    /// [`event::SharedFold::apply`] aren't counted.
    seeds: u32,
}

impl BoardFold {
    /// The fold, (re)seeded by walking the board's history when it has none or
    /// it admits a different set of channels. A seed moves the revision.
    fn seeded(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        board_addr: &str,
        channels: &[Pubkey],
        next: &mut u64,
    ) -> Option<&mut event::SharedFold> {
        if !self.fold.as_ref().is_some_and(|f| f.sealed_under(channels)) {
            self.fold = event::SharedFold::seed(ndb, txn, board_addr, channels);
            self.seeds += 1;
            self.changed(next);
        }
        self.fold.as_mut()
    }

    /// Fold what the subscription reported (`polled`), plus whatever an earlier
    /// pass deferred, into the board, seeding it first if need be. Keys `txn`
    /// can't read yet wait in [`pending`](Self::pending). Returns whether the
    /// board changed.
    fn advance(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        board_addr: &str,
        channels: &[Pubkey],
        polled: Vec<NoteKey>,
        next: &mut u64,
    ) -> bool {
        let before = self.revision;
        let mut keys = std::mem::take(&mut self.pending);
        keys.extend(polled);
        let Some(fold) = self.seeded(ndb, txn, board_addr, channels, next) else {
            self.pending = keys;
            return self.revision != before;
        };
        if keys.is_empty() {
            return self.revision != before;
        }
        let applied = fold.apply(ndb, txn, &keys);
        if applied.changed() {
            self.changed(next);
        }
        self.pending = applied.deferred;
        self.revision != before
    }

    /// The fold changed: drop the memo and move [`revision`](Self::revision)
    /// to `next`, the cache's next unused revision.
    fn changed(&mut self, next: &mut u64) {
        self.finalized = None;
        *next += 1;
        self.revision = *next;
    }
}

/// Which fold a set of [`Boards`] came out of: a board's
/// [`BoardFold::revision`]. Two equal ones are the same fold, so whatever was
/// derived from the first still holds for the second: the key a reader caches
/// derived rows on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FoldRevision(pub(crate) u64);

/// A shared handle to one fold's memoized boards, with the revision they were
/// folded at. Derefs to the boards; cloning bumps a refcount.
#[derive(Clone)]
pub(crate) struct Boards {
    boards: Rc<[BoardView]>,
    revision: FoldRevision,
}

impl Boards {
    /// `boards` as folded at `revision`, for a test that builds its own.
    #[cfg(test)]
    pub(crate) fn at(boards: Vec<BoardView>, revision: FoldRevision) -> Self {
        Self {
            boards: boards.into(),
            revision,
        }
    }
}

impl Deref for Boards {
    type Target = [BoardView];

    fn deref(&self) -> &[BoardView] {
        &self.boards
    }
}

/// Which board a [`BoardRef`] names: the fold it comes out of and its place
/// there. Copy, so a reader keeps it without keeping the fold alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BoardRevision {
    fold: FoldRevision,
    index: usize,
}

/// A zero-copy handle to one folded board: the memoized board set it lives in
/// plus its position there. Derefs to the [`BoardView`]; cloning bumps a
/// refcount rather than copying cards.
///
/// The foreground render resolves the active board through [`BoardCache`] and
/// then has to drop the cache borrow before drawing, because a description that
/// references another card resolves through the same cache mid-draw. A plain
/// `&BoardView` can't outlive that borrow, and an owned copy deep-clones every
/// card, comment and activity row each frame; this handle keeps the memo alive
/// instead.
#[derive(Clone)]
pub(crate) struct BoardRef {
    boards: Boards,
    index: usize,
}

impl BoardRef {
    /// The board `board_id` authored by `author` within `boards`, if present.
    #[cfg(test)]
    pub(crate) fn find(boards: Boards, author: &[u8; 32], board_id: &str) -> Option<Self> {
        let index = boards
            .iter()
            .position(|v| v.id == board_id && &v.author == author)?;
        Some(Self { boards, index })
    }

    /// Which fold this board comes out of, and where in it. Equal for two
    /// handles exactly when the board hasn't re-folded between them, so a
    /// reader that cached something derived from a board keeps this beside it
    /// and can tell, without looking at a card, that it still holds.
    pub(crate) fn revision(&self) -> BoardRevision {
        BoardRevision {
            fold: self.boards.revision,
            index: self.index,
        }
    }

    /// The first board in `boards` — a coordinate fold's only board — if it
    /// has one.
    fn first(boards: Boards) -> Option<Self> {
        (!boards.is_empty()).then_some(Self { boards, index: 0 })
    }
}

impl Deref for BoardRef {
    type Target = BoardView;

    fn deref(&self) -> &BoardView {
        &self.boards[self.index]
    }
}

/// One board on the account's roster, as [`BoardCache::boards`] lists it.
pub(crate) struct ListedBoard {
    /// The board owner's pubkey.
    pub(crate) owner: [u8; 32],
    /// The board's slug.
    pub(crate) slug: String,
    /// Its fold, `None` until its definition has folded in (a joined board
    /// whose envelopes haven't arrived).
    pub(crate) board: Option<BoardRef>,
}

/// What one [`BoardCache::poll`] pass found.
#[derive(Default)]
pub(crate) struct CachePoll {
    /// Some board's fold changed, so whatever draws it should redraw.
    pub(crate) changed: bool,
    /// A board subscribed on this pass and so deliberately didn't fold yet (see
    /// [`BoardCache::poll`]). The caller has to schedule another frame, or the
    /// fold waits for whatever repaints next.
    pub(crate) subscribed: bool,
}

impl BoardCache {
    /// Take the account and its key-shares (`teams`), building the
    /// [`roster`](Self::roster) from them and the account's plaintext boards.
    /// Called whenever the app's roster changes. A different roster moves
    /// [`ref_revision`](Self::ref_revision), since a ref may now name another
    /// board.
    pub(crate) fn set_roster(&mut self, ndb: &Ndb, me: &Pubkey, teams: &[teams::Team]) {
        let keyshared = keyshared_boards(teams);
        match &mut self.account {
            Some(account) if account.me == *me => account.keyshared = keyshared,
            _ => {
                if let Some(old) = self.account.take().and_then(|a| a.definitions) {
                    let _ = ndb.clone().unsubscribe(old);
                }
                self.account = Some(Account {
                    me: *me,
                    keyshared,
                    plaintext: Vec::new(),
                    definitions: None,
                    stale: true,
                });
            }
        }
        self.rebuild_roster();
    }

    /// Make `me` the account the roster lists, reading its key-shares out of
    /// `txn` when it isn't already: for a reader that may run before the app
    /// has ever set a roster (an inline chip drawn in another app while
    /// Headway was never opened). A no-op once the account is `me`.
    fn ensure_account(&mut self, ndb: &Ndb, txn: &Transaction, me: &Pubkey) {
        if self.account.as_ref().is_some_and(|a| a.me == *me) {
            return;
        }
        let teams = teams::teams_in(ndb, txn, me);
        self.set_roster(ndb, me, &teams);
    }

    /// Bring the roster up to date with the account's plaintext boards: read
    /// them again when a new board definition of the account's arrived, or
    /// the account just changed.
    ///
    /// The subscription opens inside the caller's `txn`, so a plaintext board
    /// defined in between is listed only once another definition arrives.
    /// Boards are born sealed now, so a new plaintext one is rare to begin
    /// with, and a sealed one is listed through its key-share.
    fn refresh(&mut self, ndb: &Ndb, txn: &Transaction) {
        let Some(account) = &mut self.account else {
            return;
        };
        match account.definitions {
            None => {
                account.definitions = ndb
                    .subscribe(&[event::board_definition_filter(&account.me)])
                    .ok();
            }
            Some(sub) => {
                if !drain(ndb, sub).is_empty() {
                    account.stale = true;
                }
            }
        }
        if !account.stale {
            return;
        }
        account.plaintext = event::plaintext_board_slugs(ndb, txn, &account.me);
        account.stale = false;
        self.rebuild_roster();
    }

    /// Rebuild [`roster`](Self::roster) from the account's key-shared and
    /// plaintext boards. A board both lists is the key-shared one: the channels
    /// decide how it reads. Moves the revision when the roster changed.
    fn rebuild_roster(&mut self) {
        let Some(account) = &self.account else {
            return;
        };
        let mut roster = account.keyshared.clone();
        for slug in &account.plaintext {
            if roster.iter().any(|b| b.is(account.me.bytes(), slug)) {
                continue;
            }
            roster.push(RosterBoard {
                addr: event::board_address(&account.me, slug),
                owner: *account.me.bytes(),
                slug: slug.clone(),
                channels: Vec::new(),
                shared_at: 0,
            });
        }
        if roster == self.roster {
            return;
        }
        self.roster = roster;
        self.revisions += 1;
    }

    /// The per-frame pump, called from [`update`](notedeck::App::update):
    /// advance every board folded so far and every board on the roster.
    /// Ensures each one's subscription (see
    /// [`event::SharedFold::subscription_filters`]) and folds what it reports
    /// into the board as deltas, seeding the fold first if it has none.
    ///
    /// Only a seed walks the board's history: on first touch, when the
    /// board's channels change, and when an envelope arrives whose rumor never
    /// did (see [`event::SharedFold::apply`]). An edit is one note folded in.
    pub(crate) fn poll(&mut self, ndb: &Ndb, txn: &Transaction) -> CachePoll {
        self.refresh(ndb, txn);
        for board in &self.roster {
            if !self.folds.contains_key(&board.addr) {
                self.folds.insert(board.addr.clone(), BoardFold::default());
            }
        }
        let mut poll = CachePoll::default();
        for (addr, entry) in &mut self.folds {
            // One pass per *board*, not per key-share: a board with several
            // channels (a rotation epoch, or a re-seal that ran under a fresh
            // root) has its content split across them irrecoverably, so it
            // folds — and watches — the union. A board off the roster (a
            // plaintext board a chip named) has none.
            let channels = channels_of(&self.roster, addr);
            if entry.sub.is_some() && entry.sub_channels != channels {
                // A channel joined or left: what the subscription watches and
                // what the fold admits are both out of date.
                if let Some(old) = entry.sub.take() {
                    let _ = ndb.clone().unsubscribe(old);
                }
                entry.pending.clear();
            }
            if entry.sub.is_none() {
                entry.sub = event::SharedFold::subscription_filters(addr, channels)
                    .and_then(|filters| ndb.subscribe(&filters).ok());
                if entry.sub.is_some() {
                    // Don't fold on the pass that subscribes, and drop any fold
                    // seeded before it (a render's lazy `board_at`): `txn` and
                    // that seed's snapshot were both taken before this
                    // subscription existed, and a note that committed in
                    // between is in neither — too new for the snapshot, too old
                    // for a subscription that only reports later ingests — so
                    // the fold would be one note short for good.
                    //
                    // The next toucher (the next pass here, or a read during
                    // this frame's render) seeds with a transaction opened
                    // after this one is dropped, so its snapshot postdates the
                    // subscribe.
                    entry.fold = None;
                    entry.sub_channels = channels.to_vec();
                    poll.subscribed = true;
                    continue;
                }
            }
            let polled = entry.sub.map(|sub| drain(ndb, sub)).unwrap_or_default();
            poll.changed |= entry.advance(ndb, txn, addr, channels, polled, &mut self.revisions);
        }
        poll
    }

    /// Every board on the roster, with its fold once it has one, deleted
    /// boards included (a card ref into one still resolves; the switcher
    /// leaves them out). Each fold is a handle into its memo, so this copies
    /// no cards.
    #[profiling::function]
    pub(crate) fn boards(&mut self, ndb: &Ndb, txn: &Transaction) -> Vec<ListedBoard> {
        self.refresh(ndb, txn);
        (0..self.roster.len())
            .map(|index| ListedBoard {
                owner: self.roster[index].owner,
                slug: self.roster[index].slug.clone(),
                board: self.roster_fold(ndb, txn, index),
            })
            .collect()
    }

    /// Fold (memoized) the roster's `index`th board by coordinate.
    fn roster_fold(&mut self, ndb: &Ndb, txn: &Transaction, index: usize) -> Option<BoardRef> {
        let board = self.roster.get(index)?;
        fold_board(
            &mut self.folds,
            &mut self.revisions,
            ndb,
            txn,
            &board.addr,
            &board.channels,
        )
    }

    /// The board at `owner`/`slug`, by coordinate: over the channels the
    /// roster holds for it, or, a board off the roster, over none, which
    /// folds its owner's plaintext. `None` until its definition has folded
    /// in. What the foreground board and an inline board widget draw.
    #[profiling::function]
    pub(crate) fn board_at(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        owner: &[u8; 32],
        slug: &str,
    ) -> Option<BoardRef> {
        self.refresh(ndb, txn);
        if let Some(index) = self.roster.iter().position(|b| b.is(owner, slug)) {
            return self.roster_fold(ndb, txn, index);
        }
        let addr = event::board_address(&Pubkey::new(*owner), slug);
        fold_board(&mut self.folds, &mut self.revisions, ndb, txn, &addr, &[])
    }

    /// Resolve a `headway:<slug>/<words>` ref, read by `me`, to its card's
    /// issue note id. A ref names a slug but no owner, so the boards it could
    /// mean are tried in order and the first holding a card with that word id
    /// wins:
    ///
    /// 1. `me`'s own board at `slug`;
    /// 2. a joined board at `slug` someone else owns, newest key-share first.
    ///    Two joined boards sharing a slug are told apart only by which holds
    ///    the card; when both somehow do, the board shared with us most
    ///    recently wins.
    ///
    /// Only roster boards are tried (`me`'s own plaintext boards are on it), so
    /// a ref to a slug nobody has folds nothing.
    pub(crate) fn resolve_ref(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        me: &Pubkey,
        slug: &str,
        words: &str,
    ) -> Option<NoteId> {
        self.ensure_account(ndb, txn, me);
        self.refresh(ndb, txn);
        let own = self
            .roster
            .iter()
            .position(|b| b.is(me.bytes(), slug))
            .and_then(|index| self.roster_fold(ndb, txn, index))
            .and_then(|board| event::resolve_card_by_wordid(&board, words));
        if own.is_some() {
            return own;
        }
        for index in 0..self.roster.len() {
            let board = &self.roster[index];
            if &board.owner == me.bytes() || board.slug != slug {
                continue;
            }
            let found = self
                .roster_fold(ndb, txn, index)
                .and_then(|board| event::resolve_card_by_wordid(&board, words));
            if found.is_some() {
                return found;
            }
        }
        None
    }

    /// Locate the card an issue note stands for on the board it lives on,
    /// with the roster read for `me` when one is given (see
    /// [`ensure_account`](Self::ensure_account)), else for whichever account
    /// last set it.
    ///
    /// The note's `a` tag names its *origin* board, but a cross-board move
    /// places it elsewhere, so every board on the roster is searched by
    /// coordinate and the card's live placement wins (a live column over the
    /// archive, then the newest placement). A card on no roster board is
    /// looked for on its origin board, folded by coordinate: someone else's
    /// plaintext board.
    pub(crate) fn locate_issue(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        me: Option<&Pubkey>,
        origin: &event::IssueBoard,
        issue_id: &[u8; 32],
    ) -> Option<LocatedIssue> {
        /// Where a card was found: is it in a live column, and when was it
        /// placed. Ordered so the better placement is the greater.
        #[derive(PartialEq, Eq, PartialOrd, Ord)]
        struct Placement {
            live: bool,
            placed_at: u64,
        }
        /// The best placement found so far, and the board it is on.
        struct Best {
            placement: Placement,
            board: BoardRef,
        }
        if let Some(me) = me {
            self.ensure_account(ndb, txn, me);
        }
        self.refresh(ndb, txn);
        let mut best: Option<Best> = None;
        for index in 0..self.roster.len() {
            let Some(board) = self.roster_fold(ndb, txn, index) else {
                continue;
            };
            if board.deleted {
                continue;
            }
            let Some(found) = event::card_with_column_in_board(&board, issue_id) else {
                continue;
            };
            let placement = Placement {
                live: found.column.is_some(),
                placed_at: found.card.placed_at,
            };
            if best.as_ref().is_none_or(|have| placement > have.placement) {
                best = Some(Best { placement, board });
            }
        }
        if let Some(Best { board, .. }) = best {
            return Some(LocatedIssue {
                board,
                issue_id: *issue_id,
            });
        }
        if self.on_roster(&origin.owner, origin.slug) {
            return None;
        }
        let board = self.board_at(ndb, txn, &origin.owner, origin.slug)?;
        event::card_with_column_in_board(&board, issue_id)?;
        Some(LocatedIssue {
            board,
            issue_id: *issue_id,
        })
    }

    /// Is the board at `owner`/`slug` on the roster?
    fn on_roster(&self, owner: &[u8; 32], slug: &str) -> bool {
        self.roster.iter().any(|b| b.is(owner, slug))
    }

    /// Which revision of its folds a lookup by coordinate reads: a number that
    /// moves whenever [`resolve_ref`](Self::resolve_ref),
    /// [`locate_issue`](Self::locate_issue) or [`board_at`](Self::board_at)
    /// could answer differently, and holds while they can't.
    ///
    /// Those lookups may read any board (a ref's slug can name a joined board,
    /// a card can have moved to another), so this is the cache-wide counter,
    /// which every re-fold and every roster change moves.
    pub(crate) fn ref_revision(&self) -> u64 {
        self.revisions
    }
}

/// The channels `roster` holds for the board at `addr`: none for a board off
/// the roster, which then folds as plaintext.
fn channels_of<'r>(roster: &'r [RosterBoard], addr: &str) -> &'r [Pubkey] {
    roster
        .iter()
        .find(|b| b.addr == addr)
        .map_or(&[], |b| b.channels.as_slice())
}

/// Everything `sub` has waiting, however much that is.
fn drain(ndb: &Ndb, sub: Subscription) -> Vec<NoteKey> {
    const BATCH: u32 = 256;
    let mut keys = ndb.poll_for_notes(sub, BATCH);
    let mut last = keys.len();
    while last == BATCH as usize {
        let more = ndb.poll_for_notes(sub, BATCH);
        last = more.len();
        keys.extend(more);
    }
    keys
}

/// Fold (memoized) the board at `board_addr` over `channels`, seeding it on
/// first touch, over the two fields of [`BoardCache`] it touches so a caller
/// can pass channels borrowed from another field (the roster) of the same
/// cache. Allocates the map key only on a board's first touch.
fn fold_board(
    folds: &mut HashMap<String, BoardFold>,
    revisions: &mut u64,
    ndb: &Ndb,
    txn: &Transaction,
    board_addr: &str,
    channels: &[Pubkey],
) -> Option<BoardRef> {
    if !folds.contains_key(board_addr) {
        folds.insert(board_addr.to_owned(), BoardFold::default());
    }
    let entry = folds.get_mut(board_addr)?;
    entry.seeded(ndb, txn, board_addr, channels, revisions)?;
    if entry.finalized.is_none() {
        entry.finalized = Some(entry.fold.as_ref()?.reducer().finalize().into());
    }
    // A coordinate fold folds a single board, so its finalize yields that one
    // board (empty until the board definition has arrived).
    BoardRef::first(Boards {
        boards: Rc::clone(entry.finalized.as_ref()?),
        revision: FoldRevision(entry.revision),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{BoardSummary, board_summaries, store};
    use nostrdb::{Ndb, SubscriptionStream};
    use nostrdb_net::FullKeypair;
    use notedeck_testing::fixtures::test_config;
    use std::time::{Duration, Instant};

    /// A headless harness driving a [`BoardCache`] against a bare `Ndb` — the
    /// subscription / poll / refold logic with no egui in sight. Mirrors the
    /// `store::tests::TestNdb` poll-loop pattern (ingest is async).
    pub(crate) struct TestSync {
        pub(crate) ndb: Ndb,
        _dir: tempfile::TempDir,
        pub(crate) kp: FullKeypair,
        cache: BoardCache,
    }

    impl TestSync {
        /// A fresh account with no key-shares: its boards are whatever
        /// plaintext boards it writes, until [`set_teams`](Self::set_teams).
        pub(crate) fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();
            let kp = FullKeypair::generate();
            let mut cache = BoardCache::default();
            cache.set_roster(&ndb, &kp.pubkey, &[]);
            Self {
                ndb,
                _dir: dir,
                kp,
                cache,
            }
        }

        fn secret(&self) -> [u8; 32] {
            self.kp.secret_key.secret_bytes()
        }

        /// Hand the cache this account's key-shares, as the app does when its
        /// roster changes.
        fn set_teams(&mut self, teams: &[teams::Team]) {
            self.cache.set_roster(&self.ndb, &self.kp.pubkey, teams);
        }

        /// One pump of the cache (the app's per-frame `update`), in its own
        /// transaction. Returns whether some board changed (a seed or a
        /// folded-in delta) this call.
        pub(crate) fn poll(&mut self) -> bool {
            let txn = Transaction::new(&self.ndb).unwrap();
            self.cache.poll(&self.ndb, &txn).changed
        }

        /// Pump until a pass neither subscribes nor folds anything: every board
        /// on the roster is subscribed and caught up. Bounded, since a pass
        /// subscribes each board once and seeds it once.
        fn settle(&mut self) {
            for _ in 0..8 {
                let txn = Transaction::new(&self.ndb).unwrap();
                let poll = self.cache.poll(&self.ndb, &txn);
                if !poll.changed && !poll.subscribed {
                    return;
                }
            }
            panic!("the cache kept changing with nothing new to fold");
        }

        /// Seed the demo board, returning how many events it wrote (for
        /// [`seed_and_settle`](Self::seed_and_settle) to await).
        pub(crate) fn seed(&mut self) -> usize {
            seed_demo(&self.ndb, &self.kp)
        }

        /// Seed the demo board and pump until the *whole* seed has committed
        /// and folded.
        ///
        /// Ingest is async, so quiescence is proven by count, not by inspecting
        /// the board: [`seed`](Self::seed) reports how many events it wrote and
        /// this waits until the writer has delivered them all. That is what lets
        /// the idle tests assert "no further folds" without racing a straggler,
        /// and it holds no matter which seed event lands last.
        pub(crate) async fn seed_and_settle(&mut self) {
            let mut stream = ingest_stream(&self.ndb, &self.kp.pubkey);
            let n = self.seed();
            await_ingested(&mut stream, n).await;
            self.settle();
        }

        /// Pump, then fold and pick the default board, if present.
        pub(crate) fn view(&mut self) -> Option<BoardRef> {
            self.board(store::BOARD_ID)
        }

        /// Pump, then fold and pick this account's board `board_id`: what a
        /// frame does, `update` then `render`.
        fn board(&mut self, board_id: &str) -> Option<BoardRef> {
            self.poll();
            let txn = Transaction::new(&self.ndb).unwrap();
            self.cache
                .board_at(&self.ndb, &txn, self.kp.pubkey.bytes(), board_id)
        }

        /// Every board on this account's roster, summarized for the switcher.
        fn boards(&mut self) -> Vec<BoardSummary> {
            let txn = Transaction::new(&self.ndb).unwrap();
            board_summaries(&self.cache.boards(&self.ndb, &txn))
        }

        /// How many times any board was seeded by walking its history.
        fn seeds(&self) -> u32 {
            self.cache.folds.values().map(|f| f.seeds).sum()
        }

        /// Fold until the default board satisfies `pred` (ingest is async).
        pub(crate) async fn wait<F: Fn(&BoardView) -> bool>(&mut self, pred: F) {
            self.wait_until(|t| t.view().is_some_and(|v| pred(&v)))
                .await;
        }

        /// Shared loop for [`wait`](Self::wait): pump the cache until `done`
        /// holds, awaiting the writer's own ingest
        /// notification (see [`await_ingest`]) between folds rather than polling
        /// against a wall-clock deadline — the only race-free way to wait on an
        /// async ingest. Used when the test waits for a *specific* change to
        /// appear; [`seed_and_settle`](Self::seed_and_settle) instead waits for the
        /// whole seed to commit by count.
        async fn wait_until(&mut self, done: impl Fn(&mut Self) -> bool) {
            let mut stream = ingest_stream(&self.ndb, &self.kp.pubkey);
            while !done(self) {
                await_ingest(&mut stream).await;
            }
        }
    }

    /// Open a live await-handle on `author`'s headway events. Subscribing before
    /// a wait means every note the async writer ingests *after* this point wakes
    /// [`await_ingest`], so the fold loops advance on the writer's own
    /// notification instead of a wall-clock sleep.
    pub(crate) fn ingest_stream(ndb: &Ndb, author: &Pubkey) -> SubscriptionStream {
        let sub = ndb.subscribe(&[event::headway_filter(author)]).unwrap();
        SubscriptionStream::new(ndb.clone(), sub)
    }

    /// Await the next batch of ingested notes on `stream`, returning their keys.
    async fn await_ingest(stream: &mut SubscriptionStream) -> Vec<NoteKey> {
        notedeck_testing::await_batch(stream).await
    }

    /// Drain `stream` until the async writer has delivered `n` ingested notes in
    /// total (summing each batch). `n` is the exact number of events the seed
    /// wrote (see [`seed_demo`] / [`store::seed_demo_board`]), so this returns the
    /// instant the whole seed has committed — a quiescence signal that, unlike a
    /// board-state predicate, can't be satisfied while trailing events are still
    /// in flight and doesn't depend on which event the seed writes last.
    ///
    /// Bounded: a seed count that can never be reached fails the test with the
    /// running total instead of parking the thread until CI's own timeout.
    async fn await_ingested(stream: &mut SubscriptionStream, n: usize) {
        notedeck_testing::await_notes_async(stream, n).await
    }

    pub(crate) fn total_cards(view: &BoardView) -> usize {
        view.columns.iter().map(|c| c.cards.len()).sum()
    }

    /// Seed the populated demo board for the sync tests to fold and act on. The
    /// production seed is card-less; the fixture lives in [`store::seed_demo_board`].
    /// Seeded in the past so follow-up edits (stamped with the wall clock)
    /// always sort after it. Plaintext: no channel.
    fn seed_demo(ndb: &Ndb, kp: &FullKeypair) -> usize {
        store::seed_demo_board(
            ndb,
            &kp.pubkey,
            &kp.secret_key.secret_bytes(),
            store::BOARD_ID,
            1_700_000_000,
            &mut store::NoPublish,
        )
    }

    /// A channel for a sealed board, its root registered with `ndb` so the
    /// envelopes sealed into it unwrap.
    fn channel(ndb: &Ndb, seed: u8) -> store::SnsChannel {
        let mut root = [0u8; 32];
        root[0] = seed;
        root[31] = 0x42;
        assert!(ndb.add_team_root(&root));
        store::SnsChannel {
            keys: nostrdb_net::sns::derive_sns_keys(&root).expect("keys"),
        }
    }

    /// The key-share naming `owner`'s board `slug`, sealed into the channel
    /// [`channel`] makes from `seed`.
    fn team(owner: &Pubkey, slug: &str, seed: u8) -> teams::Team {
        let mut root = [0u8; 32];
        root[0] = seed;
        root[31] = 0x42;
        teams::Team {
            team_root: hex::encode(root),
            board_addr: event::board_address(owner, slug),
            epoch: None,
            shared_at: 0,
        }
    }

    /// Polling materialises a plaintext board from events already in ndb: the
    /// account's board definitions put it on the roster, and it folds by
    /// coordinate with no channel.
    #[tokio::test]
    async fn poll_materialises_the_board() {
        let mut t = TestSync::new();
        t.poll();
        t.seed_and_settle().await;
        let view = t.view().expect("board loaded");
        assert_eq!(
            view.columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["Backlog", "Todo", "In Progress", "In Review", "Done"]
        );
        assert_eq!(view.columns[0].cards.len(), 3);
    }

    /// An edit ingested after the initial load is picked up on a later poll —
    /// the cache reflects the change, not a stale snapshot.
    #[tokio::test]
    async fn poll_reloads_on_change() {
        let mut t = TestSync::new();
        t.poll();
        // Fully materialise the seed first (Todo settles at 2 once the drag card
        // moves out), so the edit below folds against a stable board.
        t.seed_and_settle().await;

        // Apply against the cached pre-edit view (as render does).
        {
            let view = t.view().expect("board loaded");
            store::apply(
                &t.ndb,
                store::BOARD_ID,
                &view,
                &t.kp.pubkey,
                &store::Signer::new(&t.secret(), None),
                store::BoardAction::AddCard(store::NewCard {
                    col: 1,
                    title: "Fresh card".to_string(),
                    ..store::NewCard::default()
                }),
                &mut store::NoPublish,
            );
        }

        // The new card only appears if a later poll folded it in.
        t.wait(|v| {
            v.columns[1]
                .cards
                .last()
                .is_some_and(|c| c.title == "Fresh card")
        })
        .await;
        let view = t.view().expect("board loaded");
        assert_eq!(view.columns[1].cards.last().unwrap().title, "Fresh card");
    }

    /// Two plaintext boards under one account are both listed, and reading
    /// either reads its memo: no fold walks a board's history again.
    #[tokio::test]
    async fn switching_board_reads_its_memo_without_refold() {
        let mut t = TestSync::new();
        t.poll();
        let mut stream = ingest_stream(&t.ndb, &t.kp.pubkey);
        let n = t.seed()
            + store::seed_board(
                &t.ndb,
                &t.kp.pubkey,
                &t.secret(),
                "work",
                "Work",
                &mut store::NoPublish,
            );

        // Both seeds counted together, because the writer is free to commit them
        // out of order: "the 'work' board has appeared" would hold while demo
        // events were still in flight, and folding those stragglers after the
        // snapshot below is exactly what this test must not race.
        await_ingested(&mut stream, n).await;
        t.settle();

        let boards = t.boards();
        assert!(
            boards
                .iter()
                .any(|b| b.id == store::BOARD_ID && b.title == "Headway")
        );
        assert!(boards.iter().any(|b| b.id == "work" && b.title == "Work"));
        let seeds = t.seeds();

        let view = t.board("work").expect("work board");
        assert_eq!(view.id, "work");
        assert_eq!(view.title, "Work");
        assert_eq!(total_cards(&view), 0, "fresh board has no cards");
        t.view().expect("the demo board");
        assert_eq!(
            t.seeds(),
            seeds,
            "reading a board must not walk its history again"
        );
    }

    /// Once quiescent, polling with nothing new must NOT re-fold — this is the
    /// whole point of the cache (no per-frame walk of the event history).
    #[tokio::test]
    async fn poll_does_not_refold_when_idle() {
        let mut t = TestSync::new();
        t.poll();
        t.seed_and_settle().await;
        let seeds = t.seeds();

        assert!(
            !t.poll(),
            "cache re-folded with no new events — the per-frame fold is back"
        );
        assert_eq!(t.seeds(), seeds);
    }

    /// A change after the initial load is absorbed incrementally: the board's
    /// fold takes the delta, with no additional walk of its history. Guards
    /// against a regression to reload-on-every-change.
    #[tokio::test]
    async fn poll_folds_changes_as_a_delta() {
        let mut t = TestSync::new();
        t.poll();
        t.seed_and_settle().await;

        // The board is seeded once, after its subscription opened; everything
        // since is incremental.
        assert_eq!(t.seeds(), 1, "seeding should fold the history once");

        {
            let view = t.view().expect("board loaded");
            store::apply(
                &t.ndb,
                store::BOARD_ID,
                &view,
                &t.kp.pubkey,
                &store::Signer::new(&t.secret(), None),
                store::BoardAction::AddCard(store::NewCard {
                    col: 1,
                    title: "Delta card".to_string(),
                    ..store::NewCard::default()
                }),
                &mut store::NoPublish,
            );
        }
        t.wait(|v| v.columns[1].cards.len() == 3).await;

        assert_eq!(
            t.seeds(),
            1,
            "the edit triggered a full re-fold instead of a delta"
        );
    }

    /// A board written while the cache is pumping folds its history once, the
    /// pass after it reaches the roster, and takes every later event as a
    /// delta: the seed's own events arrive interleaved with the pumps and
    /// never cost a walk each.
    #[tokio::test]
    async fn board_cache_folds_once_then_deltas() {
        let mut t = TestSync::new();
        t.poll();
        let mut stream = ingest_stream(&t.ndb, &t.kp.pubkey);
        t.seed();

        // Pump each ingest in as the writer delivers it (ingest is async on a
        // writer thread), reading the roster's boards the way the switcher
        // does, until the whole board has materialised.
        loop {
            t.poll();
            let done = t
                .boards_folded()
                .iter()
                .any(|v| v.id == store::BOARD_ID && total_cards(v) == 7);
            if done {
                break;
            }
            await_ingest(&mut stream).await;
        }
        assert_eq!(
            t.seeds(),
            1,
            "board cache re-walked the history instead of folding deltas"
        );
    }

    impl TestSync {
        /// Every roster board that has folded.
        fn boards_folded(&mut self) -> Vec<BoardRef> {
            let txn = Transaction::new(&self.ndb).unwrap();
            self.cache
                .boards(&self.ndb, &txn)
                .into_iter()
                .filter_map(|b| b.board)
                .collect()
        }
    }

    /// A frame with several inline references reads one board many times, but
    /// [`BoardCache`] must [`finalize`](event::BoardReducer::finalize) its fold
    /// only *once* per fold: the first read builds the board, every later read
    /// hands out the same memo. Only a fresh fold (a new note) replaces it.
    #[tokio::test]
    async fn board_cache_finalizes_once_per_fold() {
        let mut t = TestSync::new();
        t.poll();
        t.seed_and_settle().await;
        let memo = |t: &mut TestSync| {
            let txn = Transaction::new(&t.ndb).unwrap();
            let board = t
                .cache
                .board_at(&t.ndb, &txn, t.kp.pubkey.bytes(), store::BOARD_ID)
                .expect("board");
            Rc::clone(&board.boards.boards)
        };

        // A steady frame with no new notes: many reads (what N inline references
        // cost — resolve then render each) share one memo.
        let steady = memo(&mut t);
        for _ in 0..20 {
            assert!(
                Rc::ptr_eq(&memo(&mut t), &steady),
                "reads with no intervening fold re-finalized instead of reusing the memo"
            );
            t.boards();
        }

        // A fold drops the memo; the next frame's many reads then share
        // *exactly one* finalize. Simulate what a folded-in note does.
        let addr = event::board_address(&t.kp.pubkey, store::BOARD_ID);
        t.cache.folds.get_mut(&addr).unwrap().finalized = None;
        let after = memo(&mut t);
        assert!(!Rc::ptr_eq(&after, &steady), "a fold rebuilds the memo");
        for _ in 0..20 {
            assert!(
                Rc::ptr_eq(&memo(&mut t), &after),
                "reads after a fold didn't share a single finalize"
            );
        }
    }

    /// A note committed *after* the read txn the delta was polled with — the
    /// shape of a cross-device edit landing mid-frame — must still fold in, not
    /// vanish until an app restart. The subscription drains the key immediately,
    /// but a stale snapshot can't see the note yet; the cache has to retain the
    /// key and retry it, rather than drop it (the bug this guards against).
    #[tokio::test]
    async fn board_cache_retries_deltas_committed_after_the_read_txn() {
        let mut t = TestSync::new();
        t.poll();
        t.seed_and_settle().await;
        let addr = event::board_address(&t.kp.pubkey, store::BOARD_ID);

        // Block until `sub` reports an ingest (the writer commits asynchronously).
        // Reads the subscription inbox, not the db, so it needs no transaction —
        // which lets us wait while holding a stale read txn open below.
        let wait_commit = |ndb: &Ndb, sub| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while ndb.poll_for_notes(sub, 64).is_empty() {
                assert!(Instant::now() < deadline, "note never committed");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let det = t
            .ndb
            .subscribe(&[event::headway_filter(&t.kp.pubkey)])
            .unwrap();

        // Open a snapshot, *then* commit a card after it: its note is now in the
        // subscription inbox but invisible to this older transaction.
        let view = t.view().expect("board");
        let stale = Transaction::new(&t.ndb).unwrap();
        store::apply(
            &t.ndb,
            store::BOARD_ID,
            &view,
            &t.kp.pubkey,
            &store::Signer::new(&t.secret(), None),
            store::BoardAction::AddCard(store::NewCard {
                col: 1,
                title: "Late".to_string(),
                ..store::NewCard::default()
            }),
            &mut store::NoPublish,
        );
        wait_commit(&t.ndb, det);

        // Advancing under the stale snapshot drains the card's keys but can't
        // read the notes — they must be retained, not folded and not lost.
        let poll = t.cache.poll(&t.ndb, &stale);
        drop(stale);
        assert!(!poll.changed, "a deferred-only advance reads as a no-op");
        assert!(
            !t.cache.folds[&addr].pending.is_empty(),
            "the undrained keys are retained for retry, not dropped"
        );

        // Later advances have fresh snapshots; the retained keys now resolve
        // (the card's notes may still be committing, so wait for all of them).
        // A dropped key would leave the card unassembled for good.
        t.wait(|v| v.columns[1].cards.iter().any(|c| c.title == "Late"))
            .await;
        assert!(t.cache.folds[&addr].pending.is_empty());
        assert_eq!(
            t.seeds(),
            1,
            "recovered by folding a delta, not by re-seeding the whole history"
        );
    }

    /// An envelope that commits between the caller's transaction and a board's
    /// *first* subscribe must still fold. A board only re-folds when its
    /// subscription reports something, so a board whose only envelope landed in
    /// that window would never fold at all — the board stays blank for the life of
    /// the process, not just for a frame.
    #[tokio::test]
    async fn shared_board_folds_an_envelope_that_landed_before_the_first_subscribe() {
        let mut t = TestSync::new();
        let channel = channel(&t.ndb, 0x33);
        let teams = vec![team(&t.kp.pubkey, store::BOARD_ID, 0x33)];

        // The frame opens its read transaction first, as every caller does...
        let txn = Transaction::new(&t.ndb).unwrap();

        // ...and the board's one and only envelope commits after it, while the
        // cache has still never been touched for this coordinate.
        let mut stream = ingest_stream(&t.ndb, &t.kp.pubkey);
        let cols = vec![
            event::ColumnDef::new("backlog", "Backlog"),
            event::ColumnDef::new("todo", "Todo"),
        ];
        store::ingest_signed(
            &t.ndb,
            event::build_board(store::BOARD_ID, "Shared", "", &cols),
            &store::Signer::shared(&t.secret(), &channel),
            &mut store::NoPublish,
        );
        await_ingest(&mut stream).await;

        // First touch: subscribes, from a frame whose snapshot predates the commit.
        t.set_teams(&teams);
        t.cache.poll(&t.ndb, &txn);
        drop(txn);

        // No further envelope is ever published, so nothing re-arms the fold; the
        // board has to come from a later pass seeding off a fresher snapshot.
        let view = t
            .view()
            .expect("the definition that landed before the subscribe must fold in");
        assert_eq!(view.title, "Shared");
    }

    /// The sealed-board read path: an edit applied over an SNS channel is folded
    /// by the cache by coordinate as a delta, and the stored issue is an
    /// unwrapped rumor — the same envelope the 1081 subscription reports for
    /// fan-out.
    #[tokio::test]
    async fn shared_board_folds_via_cache() {
        let mut t = TestSync::new();
        let channel = channel(&t.ndb, 0x11);
        t.set_teams(&[team(&t.kp.pubkey, store::BOARD_ID, 0x11)]);
        let addr = event::board_address(&t.kp.pubkey, store::BOARD_ID);

        // Seal the board definition into the channel (a sealed board's own
        // definition is never read in the clear).
        let cols = vec![
            event::ColumnDef::new("backlog", "Backlog"),
            event::ColumnDef::new("todo", "Todo"),
            event::ColumnDef::new("done", "Done"),
        ];
        t.poll();
        store::ingest_signed(
            &t.ndb,
            event::build_board(store::BOARD_ID, "Headway", "", &cols),
            &store::Signer::shared(&t.secret(), &channel),
            &mut store::NoPublish,
        );
        t.wait(|v| v.id == store::BOARD_ID).await;
        t.settle();
        let seeds = t.cache.folds[&addr].seeds;

        // Add a card over the SNS channel — wrapped into a 1081 envelope, ingested
        // (and unwrapped) locally.
        let view = t.view().expect("board");
        store::apply(
            &t.ndb,
            store::BOARD_ID,
            &view,
            &t.kp.pubkey,
            &store::Signer::new(&t.secret(), Some(&channel)),
            store::BoardAction::AddCard(store::NewCard {
                col: 0,
                title: "Sealed".to_string(),
                ..store::NewCard::default()
            }),
            &mut store::NoPublish,
        );
        t.wait(|v| v.columns[0].cards.iter().any(|c| c.title == "Sealed"))
            .await;
        let card = t.view().expect("board").columns[0]
            .cards
            .iter()
            .find(|c| c.title == "Sealed")
            .map(|c| c.id)
            .unwrap();

        assert_eq!(
            t.cache.folds[&addr].seeds, seeds,
            "the edit folds in as a delta, not a walk of the board's history"
        );
        let txn = Transaction::new(&t.ndb).unwrap();
        assert!(
            t.ndb.get_note_by_id(&txn, card.bytes()).unwrap().is_rumor(),
            "a shared-board edit must be stored as an unwrapped rumor"
        );
    }

    /// An own sealed board shows a card a teammate sealed into it, both in
    /// the switcher's list and to an inline chip locating the card: the one
    /// fold by coordinate is what both read, so neither can miss a card the
    /// other shows (the drift the retired author fold had).
    #[tokio::test]
    async fn own_sealed_board_shows_a_teammates_card_in_the_switcher_and_a_chip() {
        let mut t = TestSync::new();
        let channel = channel(&t.ndb, 0x51);
        t.set_teams(&[team(&t.kp.pubkey, "ours", 0x51)]);
        let mate = FullKeypair::generate();
        let cols = vec![event::ColumnDef::new("todo", "Todo")];
        t.poll();
        store::ingest_signed(
            &t.ndb,
            event::build_board("ours", "Ours", "", &cols),
            &store::Signer::shared(&t.secret(), &channel),
            &mut store::NoPublish,
        );
        t.wait_until(|t| t.board("ours").is_some()).await;

        let view = t.board("ours").expect("board");
        let mut stream = ingest_stream(&t.ndb, &mate.pubkey);
        let card = store::apply_outcome(
            &t.ndb,
            "ours",
            &view,
            &mate.pubkey,
            &store::Signer::new(&mate.secret_key.secret_bytes(), Some(&channel)),
            store::BoardAction::AddCard(store::NewCard {
                col: 0,
                title: "Mate's card".to_string(),
                ..store::NewCard::default()
            }),
            &mut store::NoPublish,
        )
        .created
        .expect("card created");
        while !t
            .board("ours")
            .is_some_and(|v| event::card_in_board(&v, card.bytes()).is_some())
        {
            await_ingest(&mut stream).await;
        }

        let listed = t.boards_folded();
        let ours = listed
            .iter()
            .find(|v| v.id == "ours")
            .expect("the switcher lists it");
        assert!(event::card_in_board(ours, card.bytes()).is_some());

        let txn = Transaction::new(&t.ndb).unwrap();
        let origin = event::IssueBoard {
            owner: *t.kp.pubkey.bytes(),
            slug: "ours",
        };
        let me = t.kp.pubkey;
        let located = t
            .cache
            .locate_issue(&t.ndb, &txn, Some(&me), &origin, card.bytes())
            .expect("a chip locates the teammate's card");
        assert_eq!(located.card().expect("card").card.title, "Mate's card");
    }

    /// Each board is listed once, by coordinate: an own board both sealed (a
    /// key-share) and carrying a stray plaintext definition is one entry and
    /// folds over its channel, and two boards sharing a slug under different
    /// owners are two.
    #[tokio::test]
    async fn roster_lists_each_coordinate_once() {
        let mut t = TestSync::new();
        let alice = FullKeypair::generate();
        let bob = FullKeypair::generate();
        let mut stream = ingest_stream(&t.ndb, &t.kp.pubkey);
        // A plaintext definition at a coordinate that is also key-shared.
        let n = store::seed_board(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            "roadmap",
            "Stray",
            &mut store::NoPublish,
        );
        await_ingested(&mut stream, n).await;
        t.set_teams(&[
            team(&t.kp.pubkey, "roadmap", 0x21),
            team(&alice.pubkey, "notes", 0x22),
            team(&bob.pubkey, "notes", 0x23),
        ]);
        t.settle();

        let txn = Transaction::new(&t.ndb).unwrap();
        let listed = t.cache.boards(&t.ndb, &txn);
        let count = |owner: &Pubkey, slug: &str| {
            listed
                .iter()
                .filter(|b| &b.owner == owner.bytes() && b.slug == slug)
                .count()
        };
        assert_eq!(count(&t.kp.pubkey, "roadmap"), 1);
        assert_eq!(count(&alice.pubkey, "notes"), 1);
        assert_eq!(count(&bob.pubkey, "notes"), 1);
        assert_eq!(listed.len(), 3);
        let roadmap = listed
            .iter()
            .find(|b| b.slug == "roadmap")
            .expect("roadmap");
        assert!(
            roadmap.board.is_none(),
            "a key-shared board reads only what is sealed: its stray plaintext \
             definition doesn't make it a board"
        );
    }
}
