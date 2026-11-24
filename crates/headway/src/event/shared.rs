//! The incremental multi-writer fold of one shared board: [`SharedFold`].
//!
//! [`fold_shared_board`](super::fold_shared_board) walks a board's whole history
//! in three phases, because what a card's overlays are keyed by (the card id)
//! is only known once the cards are. A front end that re-ran that walk on every
//! sealed edit paid O(history) per edit. [`SharedFold`] keeps what the walk
//! learned (the reducer and the card and review-record ids it admitted) so a
//! later note folds in by itself: [`SharedFold::apply`] routes each new note to
//! the board the way the three phases would have, and catches a newly-known card
//! up on the overlays that reached nostrdb before it did.
//!
//! A board with no channel folds the same way, from its owner's plaintext
//! alone (see [`shared_fold_admits`]): every board, sealed or not, folds by
//! coordinate.

use std::collections::HashSet;

use nostrdb::{Filter, Ndb, Note, NoteKey, Transaction};
use nostrdb_net::Pubkey;
use nostrdb_net::sns::SNS_ENVELOPE_KIND;

use super::kinds::{
    KIND_BLOCKERS, KIND_BOARD, KIND_COMMENT, KIND_COVER_NOTE, KIND_ISSUE, KIND_LABEL,
    KIND_PLACEMENT, KIND_RELATED, KIND_RELATION, KIND_REVIEW, KIND_REVIEW_SOURCE, KIND_SEQUENCE,
};
use super::load::{
    board_scoped_filters, card_meta_filter, comment_filter, shared_fold_admits, team_sealed,
};
use super::model::BoardCoord;
use super::parse::parse;
use super::reduce::BoardReducer;

/// The card-anchored overlay kinds: what [`card_meta_filter`] gathers, each
/// naming its card by an `e` tag.
const OVERLAY_KINDS: [u32; 8] = [
    KIND_LABEL,
    KIND_RELATION,
    KIND_SEQUENCE,
    KIND_COVER_NOTE,
    KIND_BLOCKERS,
    KIND_RELATED,
    KIND_REVIEW,
    KIND_REVIEW_SOURCE,
];

/// How many unmatched envelope ids [`SharedFold`] remembers before it forgets
/// them all (see [`SharedFold::wraps`]). Forgetting costs at most one spurious
/// reseed per forgotten envelope, so the cap only bounds memory.
const MAX_UNMATCHED_WRAPS: usize = 4096;

/// One shared board's multi-writer fold, kept so later notes fold in as deltas.
///
/// [`seed`](Self::seed) runs the same walk as
/// [`fold_shared_board`](super::fold_shared_board), and [`apply`](Self::apply)
/// folds the notes a subscription over [`subscription_filters`](Self::subscription_filters)
/// reports afterwards. The result is the reducer a full re-fold would have
/// built: [`BoardReducer::ingest`] is commutative and idempotent, and
/// [`apply`](Self::apply) admits exactly the notes the three phases would.
pub struct SharedFold {
    reducer: BoardReducer,
    /// The board coordinate, `30619:<owner>:<slug>`.
    addr: String,
    /// The board's owner and slug, parsed out of [`addr`](Self::addr).
    coord: BoardCoord,
    /// The board's channel team pubkeys, as [`shared_fold_admits`] takes them.
    teams: Vec<[u8; 32]>,
    /// Every card (issue id) admitted: what an overlay's `e` tag or a comment's
    /// `E` tag must name for the note to belong to this board.
    cards: HashSet<[u8; 32]>,
    /// Every review record admitted: what an inline review comment's `E` tag
    /// names.
    records: HashSet<[u8; 32]>,
    /// Every card an admitted placement names: an issue anchored at another
    /// board (its `a` tag records where it was created) belongs to this one
    /// too once a placement moves or links it here, so it is gathered by id.
    placed: HashSet<[u8; 32]>,
    /// Envelope ids of sealed rumors [`apply`](Self::apply) has been handed but
    /// whose envelope it hasn't seen yet. Each envelope should find its rumor
    /// here; one that doesn't carried a note nostrdb wrote without notifying any
    /// subscription (a plaintext note promoted to a sealed rumor in place, which
    /// may now be admitted), and so reseeds the fold.
    wraps: HashSet<[u8; 32]>,
}

/// What one [`SharedFold::apply`] did.
#[derive(Debug, Default)]
pub struct Applied {
    /// How many notes it folded in as deltas (including the overlays a newly
    /// known card caught up on).
    pub folded: usize,
    /// Whether it threw the fold away and walked the board's whole history
    /// again, because an envelope arrived whose rumor never did.
    pub reseeded: bool,
    /// Keys `txn` couldn't read yet, because they committed after it opened:
    /// hand them to a later [`apply`](SharedFold::apply) with a fresher txn.
    pub deferred: Vec<NoteKey>,
}

impl Applied {
    /// Whether the reducer changed, so a memoized finalize is stale.
    pub fn changed(&self) -> bool {
        self.folded > 0 || self.reseeded
    }
}

/// Ids a walk admitted for the first time, whose own overlays still have to be
/// gathered (see [`SharedFold::catch_up`]).
#[derive(Default)]
struct Found {
    /// New cards: gather their overlays and comments.
    cards: Vec<[u8; 32]>,
    /// New review records: gather their inline comments.
    records: Vec<[u8; 32]>,
    /// Cards newly named by a placement: gather their issues.
    placed: Vec<[u8; 32]>,
}

impl Found {
    fn is_empty(&self) -> bool {
        self.cards.is_empty() && self.records.is_empty() && self.placed.is_empty()
    }
}

impl SharedFold {
    /// Fold the board at `board_addr` over `team_pubkeys` from scratch: the
    /// walk [`fold_shared_board`](super::fold_shared_board) documents. `None`
    /// if `board_addr` isn't a board coordinate or the index walk fails.
    #[profiling::function]
    pub fn seed(
        ndb: &Ndb,
        txn: &Transaction,
        board_addr: &str,
        team_pubkeys: &[Pubkey],
    ) -> Option<Self> {
        let phase_a = board_scoped_filters(board_addr)?;
        let mut fold = SharedFold {
            reducer: BoardReducer::team_authored(),
            addr: board_addr.to_owned(),
            coord: BoardCoord::parse(board_addr)?,
            teams: team_pubkeys.iter().map(|k| *k.bytes()).collect(),
            cards: HashSet::new(),
            records: HashSet::new(),
            placed: HashSet::new(),
            wraps: HashSet::new(),
        };
        let mut found = Found::default();
        fold.walk(ndb, txn, &phase_a, &mut found)?;
        fold.catch_up(ndb, txn, found)?;
        Some(fold)
    }

    /// The filters a subscription feeding [`apply`](Self::apply) needs, so
    /// that every note the board could come to hold is reported: the channels'
    /// kind-1081 envelopes (none for a plaintext board), the board definition,
    /// every placement at the coordinate, and every card, card-anchored overlay
    /// and comment, whatever board or card it names (which ones belong is
    /// [`apply`](Self::apply)'s check: a card can arrive after a placement
    /// moved it here, and a filter keyed on card ids would have to be replaced
    /// on every new card). Unbounded, since a subscription's limit caps nothing
    /// useful. `None` if `board_addr` isn't a board coordinate.
    ///
    /// Matching every overlay and comment kind means the subscription sees
    /// other boards' notes too, so drain it every pass: nostrdb drops what
    /// doesn't fit in a subscription's inbox without saying so, and a dropped
    /// note stays out of the fold until its next reseed.
    pub fn subscription_filters(board_addr: &str, team_pubkeys: &[Pubkey]) -> Option<Vec<Filter>> {
        let coord = BoardCoord::parse(board_addr)?;
        let mut filters = Vec::with_capacity(5);
        if !team_pubkeys.is_empty() {
            filters.push(
                Filter::new()
                    .kinds([SNS_ENVELOPE_KIND as u64])
                    .authors(team_pubkeys.iter().map(|k| k.bytes()))
                    .build(),
            );
        }
        filters.extend([
            Filter::new()
                .kinds([KIND_BOARD as u64])
                .authors([&coord.owner])
                .tags([coord.slug.as_str()], 'd')
                .build(),
            Filter::new()
                .kinds([KIND_PLACEMENT as u64])
                .tags([board_addr], 'a')
                .build(),
            Filter::new()
                .kinds(
                    std::iter::once(KIND_ISSUE)
                        .chain(OVERLAY_KINDS)
                        .map(u64::from),
                )
                .build(),
            Filter::new().kinds([KIND_COMMENT as u64]).build(),
        ]);
        Some(filters)
    }

    /// The folded reducer.
    pub fn reducer(&self) -> &BoardReducer {
        &self.reducer
    }

    /// The folded reducer, by value.
    pub fn into_reducer(self) -> BoardReducer {
        self.reducer
    }

    /// Whether this fold admits notes sealed under exactly `team_pubkeys`. A
    /// board whose channels changed has to be [`seed`](Self::seed)ed again:
    /// notes sealed under a new channel were refused until now.
    pub fn sealed_under(&self, team_pubkeys: &[Pubkey]) -> bool {
        self.teams.len() == team_pubkeys.len()
            && self
                .teams
                .iter()
                .zip(team_pubkeys)
                .all(|(held, given)| held == given.bytes())
    }

    /// Fold the notes at `keys` (what a subscription over
    /// [`subscription_filters`](Self::subscription_filters) reported, plus
    /// anything an earlier call deferred) into the board.
    ///
    /// Each note is admitted the way the three-phase walk would have admitted
    /// it, and a card or review record seen for the first time catches up on
    /// the overlays that reached nostrdb before it did. Envelopes fold nothing
    /// themselves; one whose rumor never arrived reseeds the whole fold (see
    /// [`wraps`](Self::wraps)), which is the only time this walks the history.
    #[must_use = "deferred keys must be retried with a fresher txn, not dropped"]
    #[profiling::function]
    pub fn apply(&mut self, ndb: &Ndb, txn: &Transaction, keys: &[NoteKey]) -> Applied {
        let mut applied = Applied::default();
        let mut found = Found::default();
        // Envelopes are checked after every rumor in the batch is in `wraps`:
        // a batch can hand the envelope over before its rumor.
        let mut envelopes: Vec<[u8; 32]> = Vec::new();
        for key in keys {
            let Ok(note) = ndb.get_note_by_key(txn, *key) else {
                applied.deferred.push(*key);
                continue;
            };
            if note.kind() == SNS_ENVELOPE_KIND {
                envelopes.push(*note.id());
                continue;
            }
            if team_sealed(&note, &self.teams)
                && let Some(wrap) = note.rumor_giftwrap_id()
            {
                self.wraps.insert(*wrap);
            }
            if self.admit(&note, &mut found) {
                applied.folded += 1;
            }
        }
        applied.folded += self.catch_up(ndb, txn, found).unwrap_or(0);

        let unmatched = envelopes.iter().any(|id| !self.wraps.remove(id));
        if self.wraps.len() > MAX_UNMATCHED_WRAPS {
            self.wraps.clear();
        }
        if unmatched && let Some(fresh) = Self::seed_over(ndb, txn, &self.addr, &self.teams) {
            *self = fresh;
            applied.reseeded = true;
        }
        applied
    }

    /// [`seed`](Self::seed) over team pubkeys already in byte form.
    fn seed_over(
        ndb: &Ndb,
        txn: &Transaction,
        board_addr: &str,
        teams: &[[u8; 32]],
    ) -> Option<Self> {
        let pubkeys: Vec<Pubkey> = teams.iter().copied().map(Pubkey::new).collect();
        Self::seed(ndb, txn, board_addr, &pubkeys)
    }

    /// Fold every note `filters` match into the board, collecting the cards
    /// and review records admitted for the first time into `found`. Returns
    /// how many notes it folded, or `None` if the index walk failed.
    fn walk(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        filters: &[Filter],
        found: &mut Found,
    ) -> Option<usize> {
        ndb.fold(txn, filters, 0, |folded, note| {
            folded + usize::from(self.admit(&note, found))
        })
        .ok()
    }

    /// Gather the overlays and comments of the cards and records in `found`
    /// (phases B and C of the walk), the issues of the cards its placements
    /// name, and the same for whatever those turn up, until nothing new is
    /// found. Returns how many notes it folded, or `None`
    /// if an index walk failed.
    fn catch_up(&mut self, ndb: &Ndb, txn: &Transaction, mut found: Found) -> Option<usize> {
        let mut folded = 0;
        while !found.is_empty() {
            let mut filters = Vec::with_capacity(4);
            if !found.placed.is_empty() {
                filters.push(
                    Filter::new()
                        .ids(found.placed.iter())
                        .kinds([KIND_ISSUE as u64])
                        .build(),
                );
            }
            if !found.cards.is_empty() {
                filters.push(card_meta_filter(&found.cards));
                filters.push(comment_filter(&found.cards));
            }
            if !found.records.is_empty() {
                filters.push(comment_filter(&found.records));
            }
            let mut next = Found::default();
            folded += self.walk(ndb, txn, &filters, &mut next)?;
            found = next;
        }
        Some(folded)
    }

    /// Fold `note` in if it belongs to this board and the board trusts its
    /// author (see [`shared_fold_admits`]). A card or review record seen for
    /// the first time goes into `found`, so its own overlays get gathered.
    /// Returns whether it folded.
    fn admit(&mut self, note: &Note, found: &mut Found) -> bool {
        if !shared_fold_admits(note, &self.coord.owner, &self.teams) || !self.belongs(note) {
            return false;
        }
        let (known, new) = match note.kind() {
            KIND_ISSUE => (&mut self.cards, &mut found.cards),
            KIND_REVIEW => (&mut self.records, &mut found.records),
            KIND_PLACEMENT => {
                self.note_placed(note, found);
                return self.ingest(note);
            }
            _ => return self.ingest(note),
        };
        if known.insert(*note.id()) {
            new.push(*note.id());
        }
        self.ingest(note)
    }

    /// Remember the card an admitted `placement` names, and put it in `found`
    /// when its issue hasn't been gathered yet, so a card moved or linked here
    /// from another board is gathered by id.
    fn note_placed(&mut self, placement: &Note, found: &mut Found) {
        let card = placement
            .tags()
            .iter()
            .find(|tag| tag.get_str(0) == Some("e"))
            .and_then(|tag| tag.get_id(1).copied());
        let Some(card) = card else {
            return;
        };
        if !self.cards.contains(&card) && self.placed.insert(card) {
            found.placed.push(card);
        }
    }

    /// Parse `note` and fold it into the reducer. Returns whether it folded.
    fn ingest(&mut self, note: &Note) -> bool {
        let Some(event) = parse(note) else {
            return false;
        };
        self.reducer.ingest(event);
        true
    }

    /// Whether `note` is one of this board's, by the rule the walk's filters
    /// apply: the owner's definition at the coordinate, a card or placement
    /// `a`-tagged with it, a card one of its placements names, an overlay whose
    /// `e` names one of its cards, or a comment whose root `E` names one of its
    /// cards or review records.
    fn belongs(&self, note: &Note) -> bool {
        match note.kind() {
            KIND_BOARD => {
                note.pubkey() == &self.coord.owner
                    && has_tag(note, "d", |tag| tag.get_str(1) == Some(&self.coord.slug))
            }
            KIND_ISSUE => {
                self.placed.contains(note.id())
                    || has_tag(note, "a", |tag| tag.get_str(1) == Some(&self.addr))
            }
            KIND_PLACEMENT => has_tag(note, "a", |tag| tag.get_str(1) == Some(&self.addr)),
            KIND_COMMENT => has_tag(note, "E", |tag| {
                tag.get_id(1)
                    .is_some_and(|id| self.cards.contains(id) || self.records.contains(id))
            }),
            kind if OVERLAY_KINDS.contains(&kind) => has_tag(note, "e", |tag| {
                tag.get_id(1).is_some_and(|id| self.cards.contains(id))
            }),
            _ => false,
        }
    }
}

/// Whether `note` has a tag named `name` that `matches`.
fn has_tag(note: &Note, name: &str, matches: impl Fn(&nostrdb::Tag) -> bool) -> bool {
    note.tags()
        .iter()
        .any(|tag| tag.get_str(0) == Some(name) && matches(&tag))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::build::{
        build_board, build_comment, build_cover_note, build_issue, build_placement, build_review,
        build_review_comment, build_subject_edit,
    };
    use crate::event::load::fold_shared_board;
    use crate::event::model::{ColumnDef, ReviewFields, board_address};
    use crate::event::view::BoardView;
    use crate::store::{self, NoPublish, Signer, SnsChannel};
    use crate::test_config;
    use nostrdb::{NoteBuilder, Subscription};
    use nostrdb_net::{FullKeypair, NoteId};
    use std::time::{Duration, Instant};

    /// A sealed board in a fresh nostrdb, with a subscription over
    /// [`SharedFold::subscription_filters`] opened before anything is written,
    /// the way the app's cache opens it.
    struct Board {
        _dir: tempfile::TempDir,
        ndb: Ndb,
        owner: FullKeypair,
        addr: String,
        channel: SnsChannel,
        sub: Subscription,
    }

    impl Board {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();
            let owner = FullKeypair::generate();
            let addr = board_address(&owner.pubkey, "team");
            let mut root = [0u8; 32];
            root[0] = 0x5a;
            root[31] = 0x17;
            let channel = SnsChannel {
                keys: nostrdb_net::sns::derive_sns_keys(&root).expect("derive sns keys"),
            };
            assert!(ndb.add_team_root(&root));
            let filters = SharedFold::subscription_filters(&addr, &[Self::team(&channel)]).unwrap();
            let sub = ndb.subscribe(&filters).unwrap();
            Board {
                _dir: dir,
                ndb,
                owner,
                addr,
                channel,
                sub,
            }
        }

        fn team(channel: &SnsChannel) -> Pubkey {
            channel.keys.team_keypair.pubkey
        }

        fn teams(&self) -> [Pubkey; 1] {
            [Self::team(&self.channel)]
        }

        /// Seal `b`, signed by `who`, into the board's channel.
        fn seal(&self, who: &FullKeypair, b: NoteBuilder) -> NoteId {
            store::ingest_signed(
                &self.ndb,
                b,
                &Signer::shared(&who.secret_key.secret_bytes(), &self.channel),
                &mut NoPublish,
            )
            .expect("sealed ingest")
        }

        /// The board a full three-phase walk folds right now.
        fn full(&self) -> Option<BoardView> {
            let txn = Transaction::new(&self.ndb).unwrap();
            fold_shared_board(&self.ndb, &txn, &self.addr, &self.teams())?
                .finalize()
                .into_iter()
                .next()
        }

        /// Drain the subscription into `fold` until `done` holds of its board,
        /// returning every [`Applied`] along the way.
        fn drain_until(
            &self,
            fold: &mut SharedFold,
            done: impl Fn(&BoardView) -> bool,
        ) -> Vec<Applied> {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut pending: Vec<NoteKey> = Vec::new();
            let mut log = Vec::new();
            loop {
                let txn = Transaction::new(&self.ndb).unwrap();
                pending.extend(self.ndb.poll_for_notes(self.sub, 1024));
                let applied = fold.apply(&self.ndb, &txn, &std::mem::take(&mut pending));
                pending = applied.deferred.clone();
                log.push(applied);
                if fold.reducer().finalize().first().is_some_and(&done) {
                    return log;
                }
                assert!(Instant::now() < deadline, "the delta fold never settled");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    /// A plaintext board, one with no channel, folds by coordinate from its
    /// owner's signed plaintext, definition included, and from no one
    /// else's. A card moved onto it from another of the owner's plaintext
    /// boards, whose issue is `a`-tagged with the board it was created on, is
    /// gathered by the placement that moved it: as a delta, and by a seed.
    #[test]
    fn a_plaintext_board_folds_its_owners_notes_and_moved_in_cards() {
        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();
        let owner = FullKeypair::generate();
        let stranger = FullKeypair::generate();
        let plain = |who: &FullKeypair, b: NoteBuilder| {
            store::ingest_signed(
                &ndb,
                b,
                &Signer::new(&who.secret_key.secret_bytes(), None),
                &mut NoPublish,
            )
            .expect("plaintext ingest")
        };
        let settle = |done: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !done() {
                assert!(Instant::now() < deadline, "the notes never landed");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let full = |addr: &str| {
            let txn = Transaction::new(&ndb).unwrap();
            fold_shared_board(&ndb, &txn, addr, &[])?
                .finalize()
                .into_iter()
                .next()
        };
        let a = board_address(&owner.pubkey, "a");
        let b = board_address(&owner.pubkey, "b");
        let cols = vec![ColumnDef::new("todo", "Todo")];
        let sub = ndb
            .subscribe(&SharedFold::subscription_filters(&b, &[]).unwrap())
            .unwrap();

        plain(&owner, build_board("a", "A", "", &cols));
        plain(&owner, build_board("b", "B", "", &cols));
        let card = plain(&owner, build_issue(&a, "Moved", ""));
        settle(&|| full(&b).is_some_and(|v| v.title == "B"));
        assert_eq!(
            full(&a).map(|v| v.title),
            Some("A".to_owned()),
            "a board with no channel reads its owner's plaintext definition"
        );

        // Seeded before the move, so the card can only arrive as a delta.
        let mut fold = {
            let txn = Transaction::new(&ndb).unwrap();
            SharedFold::seed(&ndb, &txn, &b, &[]).expect("seed")
        };
        let _ = ndb.poll_for_notes(sub, 1024);
        assert!(card_count(&view(&fold)) == 0);

        // A stranger's plaintext card placed on the board is refused.
        let squat = plain(&stranger, build_issue(&b, "Squatted", ""));
        plain(&stranger, build_placement("b", &b, &squat, "todo", "a"));
        plain(&owner, build_placement("b", &b, &card, "todo", "m"));
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut pending: Vec<NoteKey> = Vec::new();
        while card_count(&view(&fold)) == 0 {
            assert!(Instant::now() < deadline, "the moved card never folded in");
            std::thread::sleep(Duration::from_millis(10));
            let txn = Transaction::new(&ndb).unwrap();
            pending.extend(ndb.poll_for_notes(sub, 1024));
            pending = fold.apply(&ndb, &txn, &pending).deferred;
        }
        let titles = |v: &BoardView| -> Vec<String> {
            v.columns
                .iter()
                .flat_map(|c| &c.cards)
                .map(|c| c.title.clone())
                .collect()
        };
        assert_eq!(titles(&view(&fold)), ["Moved"]);
        settle(&|| full(&b).is_some_and(|v| card_count(&v) == 1));
        assert_eq!(titles(&full(&b).unwrap()), ["Moved"], "and by a seed");
    }

    /// A plaintext board is public: a stranger's comment on a card, and on a
    /// review record of it, shows attributed to the stranger, by a seed and
    /// as a delta. Anything else the stranger signs stays out, though it is
    /// newer than the owner's: a rename, a move to another column, and a
    /// placement of the owner's card on the board under the stranger's name.
    #[test]
    fn a_plaintext_board_shows_a_strangers_comment_and_nothing_else() {
        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();
        let owner = FullKeypair::generate();
        let stranger = FullKeypair::generate();
        let plain = |who: &FullKeypair, b: NoteBuilder| {
            store::ingest_signed(
                &ndb,
                b,
                &Signer::new(&who.secret_key.secret_bytes(), None),
                &mut NoPublish,
            )
            .expect("plaintext ingest")
        };
        let addr = board_address(&owner.pubkey, "open");
        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];
        let sub = ndb
            .subscribe(&SharedFold::subscription_filters(&addr, &[]).unwrap())
            .unwrap();

        plain(
            &owner,
            build_board("open", "Open", "", &cols).created_at(1_000),
        );
        let card = plain(&owner, build_issue(&addr, "Owned", "").created_at(1_001));
        plain(
            &owner,
            build_placement("open", &addr, &card, "todo", "m").created_at(1_002),
        );
        let fields = ReviewFields {
            commit: Some("abc123".into()),
            ..Default::default()
        };
        let record = plain(&owner, build_review(&card, &fields).created_at(1_003));

        // Seeded before the stranger writes, so their notes arrive as deltas.
        let mut fold = {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let txn = Transaction::new(&ndb).unwrap();
                if let Some(fold) = SharedFold::seed(&ndb, &txn, &addr, &[])
                    && fold.reducer().finalize().first().is_some_and(|v| {
                        v.columns[0]
                            .cards
                            .first()
                            .is_some_and(|c| !c.reviews.is_empty())
                    })
                {
                    break fold;
                }
                assert!(Instant::now() < deadline, "the owner's board never landed");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let _ = ndb.poll_for_notes(sub, 1024);

        plain(
            &stranger,
            build_subject_edit(&card, "Hijacked").created_at(2_000),
        );
        plain(
            &stranger,
            build_placement("open", &addr, &card, "done", "a").created_at(2_001),
        );
        let comment = plain(
            &stranger,
            build_comment(&card, &owner.pubkey, None, "drive-by", &[]).created_at(2_002),
        );
        let review_comment = plain(
            &stranger,
            build_review_comment(&record, &owner.pubkey, None, None, "nit").created_at(2_003),
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut pending: Vec<NoteKey> = Vec::new();
        loop {
            let txn = Transaction::new(&ndb).unwrap();
            pending.extend(ndb.poll_for_notes(sub, 1024));
            pending = fold.apply(&ndb, &txn, &pending).deferred;
            let v = view(&fold);
            let shown = v.columns[0]
                .cards
                .first()
                .is_some_and(|c| c.comments.len() == 1 && c.reviews[0].comments.len() == 1);
            if shown {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the stranger's comments never folded in"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        let seeded = {
            let txn = Transaction::new(&ndb).unwrap();
            fold_shared_board(&ndb, &txn, &addr, &[])
                .expect("seed")
                .finalize()
                .into_iter()
                .next()
                .expect("a board")
        };
        for (how, v) in [("as a delta", view(&fold)), ("by a seed", seeded)] {
            assert_eq!(card_count(&v), 1, "{how}");
            assert!(
                v.columns[1].cards.is_empty(),
                "{how}: the stranger's move stays out"
            );
            let shown = &v.columns[0].cards[0];
            assert_eq!(
                shown.title, "Owned",
                "{how}: the stranger's rename stays out"
            );
            assert_eq!(shown.comments.len(), 1, "{how}");
            assert_eq!(shown.comments[0].id, comment, "{how}");
            assert_eq!(shown.comments[0].body, "drive-by", "{how}");
            assert_eq!(
                shown.comments[0].author,
                *stranger.pubkey.bytes(),
                "{how}: attributed to its signer"
            );
            let inline = &shown.reviews[0].comments;
            assert_eq!(inline.len(), 1, "{how}");
            assert_eq!(inline[0].id, review_comment, "{how}");
            assert_eq!(inline[0].author, *stranger.pubkey.bytes(), "{how}");
        }
    }

    /// How many cards `view` shows across its columns.
    fn card_count(view: &BoardView) -> usize {
        view.columns.iter().map(|c| c.cards.len()).sum()
    }

    fn view(fold: &SharedFold) -> BoardView {
        fold.reducer()
            .finalize()
            .into_iter()
            .next()
            .expect("a board")
    }

    /// Every note a later edit can add — a card, its placement, a subject
    /// edit, a cover note, a comment and a threaded reply, a review record and
    /// an inline comment on it, and an overlay that reaches nostrdb *before*
    /// its card — folds in as a delta, and the board comes out equal to a full
    /// walk's, without one reseed.
    #[test]
    fn deltas_fold_to_what_a_full_walk_folds() {
        let b = Board::new();
        let member = FullKeypair::generate();
        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];
        b.seal(
            &b.owner,
            build_board("team", "Team", "", &cols).created_at(1_000),
        );
        let txn = Transaction::new(&b.ndb).unwrap();
        let mut fold = SharedFold::seed(&b.ndb, &txn, &b.addr, &b.teams()).unwrap();
        drop(txn);
        b.drain_until(&mut fold, |v| v.title == "Team");

        let a = b.seal(
            &member,
            build_issue(&b.addr, "Card A", "").created_at(1_001),
        );
        b.seal(
            &member,
            build_placement("team", &b.addr, &a, "todo", "g").created_at(1_001),
        );
        b.seal(
            &b.owner,
            build_subject_edit(&a, "Card A, renamed").created_at(1_002),
        );
        b.seal(
            &member,
            build_cover_note(&a, &member.pubkey, "the body", &[]).created_at(1_002),
        );
        let top = b.seal(
            &member,
            build_comment(&a, &member.pubkey, None, "top", &[]).created_at(1_003),
        );
        b.seal(
            &b.owner,
            build_comment(
                &a,
                &member.pubkey,
                Some((&top, &member.pubkey)),
                "reply",
                &[],
            )
            .created_at(1_004),
        );
        let fields = ReviewFields {
            commit: Some("abc123".into()),
            ..Default::default()
        };
        let record = b.seal(&member, build_review(&a, &fields).created_at(1_005));
        b.seal(
            &b.owner,
            build_review_comment(&record, &member.pubkey, None, None, "nit").created_at(1_006),
        );

        // Card B's cover note is sealed before card B itself, so when it
        // arrives the fold doesn't know B yet: B's arrival has to catch it up.
        let issue_b = || build_issue(&b.addr, "Card B", "").created_at(1_007);
        let b_id = NoteId::new(
            *issue_b()
                .sign(&member.secret_key.secret_bytes())
                .build()
                .unwrap()
                .id(),
        );
        b.seal(
            &member,
            build_cover_note(&b_id, &member.pubkey, "early", &[]).created_at(1_007),
        );
        assert_eq!(b.seal(&member, issue_b()), b_id);
        b.seal(
            &member,
            build_placement("team", &b.addr, &b_id, "done", "g").created_at(1_008),
        );

        let log = b.drain_until(&mut fold, |v| {
            v.columns.iter().map(|c| c.cards.len()).sum::<usize>() == 2
                && v.columns
                    .iter()
                    .flat_map(|c| &c.cards)
                    .all(|card| !card.description.is_empty())
                && v.columns[0].cards[0].comments.len() == 2
                && v.columns[0].cards[0]
                    .reviews
                    .first()
                    .is_some_and(|r| r.comments.len() == 1)
        });
        assert!(
            log.iter().all(|a| !a.reseeded),
            "every edit's rumor arrived with its envelope, so nothing reseeds"
        );
        assert_eq!(view(&fold), b.full().unwrap());
        assert_eq!(view(&fold).columns[0].cards[0].title, "Card A, renamed");
        assert_eq!(view(&fold).columns[1].cards[0].description, "early");
    }

    /// A stranger's plaintext card at the coordinate stays out, and when the
    /// stranger seals that same note into the channel nostrdb promotes it in
    /// place without telling any subscription. Only its envelope arrives, so
    /// the fold reseeds and the card shows up.
    #[test]
    fn an_envelope_without_its_rumor_reseeds() {
        let b = Board::new();
        let cols = vec![ColumnDef::new("todo", "Todo")];
        b.seal(
            &b.owner,
            build_board("team", "Team", "", &cols).created_at(1_000),
        );
        let txn = Transaction::new(&b.ndb).unwrap();
        let mut fold = SharedFold::seed(&b.ndb, &txn, &b.addr, &b.teams()).unwrap();
        drop(txn);
        b.drain_until(&mut fold, |v| v.title == "Team");

        let stranger = FullKeypair::generate();
        let card = || build_issue(&b.addr, "Promoted", "").created_at(1_001);
        let placement =
            |id: &NoteId| build_placement("team", &b.addr, id, "todo", "g").created_at(1_001);
        let id = store::ingest(
            &b.ndb,
            card(),
            &stranger.secret_key.secret_bytes(),
            &mut NoPublish,
        )
        .unwrap();
        store::ingest(
            &b.ndb,
            placement(&id),
            &stranger.secret_key.secret_bytes(),
            &mut NoPublish,
        )
        .unwrap();
        // Let the plaintext land and be refused.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen = 0;
        while seen < 2 {
            seen += b.ndb.poll_for_notes(b.sub, 16).len();
            assert!(Instant::now() < deadline, "plaintext never landed");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(view(&fold).columns[0].cards.is_empty());

        assert_eq!(b.seal(&stranger, card()), id);
        b.seal(&stranger, placement(&id));
        let log = b.drain_until(&mut fold, |v| v.columns[0].cards.len() == 1);
        assert!(
            log.iter().any(|a| a.reseeded),
            "a promote can only be seen by reseeding"
        );
        assert_eq!(view(&fold), b.full().unwrap());
    }
}
