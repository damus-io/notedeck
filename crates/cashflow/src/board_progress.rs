//! How the work on a Headway board is moving: what is in progress, what is
//! in review, and when each finished card was finished.
//!
//! A project links to boards in the vault
//! ([`crate::settings::set_project_boards`]); the Projects view reads each
//! linked board's [`BoardProgress`] to show the work beside the revenue.
//!
//! Boards are read through the `headway` core crate ([`headway::boards`]),
//! as [`crate::goals`] reads them, so a sealed or shared board folds like a
//! plain one.
//!
//! ## When a card was finished
//!
//! A card is finished when it sits in one of the board's terminal columns
//! ([`BoardView::column_is_terminal`]: In Review and Done on a board that
//! marks both). It was finished when it last *entered* the terminal
//! columns: [`completed_at`] walks the card's move history
//! ([`CardView::activity`]) for the start of the run of terminal columns
//! it ends in, so a card moved from In Review to Done was finished when it
//! reached In Review. A card that never moved was finished when it was
//! made. Terminal is judged by the board's columns as they are now, so a
//! column marked terminal later counts its past arrivals too. Headway's
//! history leaves out moves made in the second a card was created, so a
//! card made and moved straight to Done dates from its creation.
//!
//! An archived card counts when it was archived from a terminal column, at
//! the time it got there: archiving a finished card tidies the board, it
//! doesn't undo the work.

use headway::boards::{self, Roster};
use headway::event::{ActivityKind, BoardView, CardView, ColumnView};
use nostrdb::Ndb;

/// The id of the default board's "waiting for review" column.
const IN_REVIEW_COLUMN: &str = "in-review";

/// The name of the default board's "waiting for review" column.
const IN_REVIEW_NAME: &str = "in review";

/// One board's progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardProgress {
    /// The board's slug.
    pub board: String,
    /// The board's owner: a slug is only unique per owner.
    pub owner: [u8; 32],
    /// How many cards sit in its In Progress column.
    pub in_progress: usize,
    /// How many cards sit in its In Review column.
    pub in_review: usize,
    /// When each finished card was finished, in unix seconds, oldest
    /// first: see the module docs.
    pub completed: Vec<u64>,
}

/// The boards called `slugs`, in that order: for each slug the account's
/// own board, then every board of that slug shared with it. A deleted
/// board, or one not in the cache, is left out.
///
/// The roster must have been [loaded](Roster::load) against `ndb`, which is
/// what registers the sealed boards' roots: without it they fold empty.
#[profiling::function]
pub fn named_boards(ndb: &Ndb, roster: &Roster, slugs: &[impl AsRef<str>]) -> Vec<BoardView> {
    let mut shared = boards::list_shared_with_me(ndb, roster);
    let mut views = Vec::new();
    for slug in slugs {
        let slug = slug.as_ref();
        let own = boards::load_board(ndb, roster, slug).filter(|view| !view.deleted);
        views.extend(own);
        let mut at = 0;
        while at < shared.len() {
            if shared[at].id == slug {
                views.push(shared.remove(at));
            } else {
                at += 1;
            }
        }
    }
    views
}

/// The progress of the boards called `slugs`: see [`named_boards`].
pub fn load(ndb: &Ndb, roster: &Roster, slugs: &[impl AsRef<str>]) -> Vec<BoardProgress> {
    named_boards(ndb, roster, slugs)
        .iter()
        .map(progress)
        .collect()
}

/// One folded board's progress.
#[profiling::function]
pub fn progress(view: &BoardView) -> BoardProgress {
    let terminal = |column: &str| is_terminal(view, column);
    let mut completed: Vec<u64> = Vec::new();
    let mut in_progress = 0;
    let mut in_review = 0;
    for column in &view.columns {
        if column.is_in_progress() {
            in_progress += column.cards.len();
        }
        if is_in_review(column) {
            in_review += column.cards.len();
        }
        if !view.column_is_terminal(&column.id) {
            continue;
        }
        let finished = column.cards.iter();
        completed.extend(finished.filter_map(|card| completed_at(card, true, terminal)));
    }
    for archived in &view.archived {
        let from_terminal = archived
            .from
            .as_deref()
            .is_some_and(|from| view.column_is_terminal(from));
        if !from_terminal {
            continue;
        }
        completed.extend(completed_at(&archived.card, true, terminal));
    }
    completed.sort_unstable();
    BoardProgress {
        board: view.id.clone(),
        owner: view.author,
        in_progress,
        in_review,
        completed,
    }
}

/// When `card` was finished, if it is: the start of the run of terminal
/// columns its move history ends in. `ends_terminal` is whether the column
/// it sits in now (or was archived from) is terminal; `terminal` says
/// whether a column, by the name or id its history gives, is.
///
/// A card whose history has no moves has sat in one column since it was
/// made, so it was finished then. A card whose first move left a terminal
/// column was made in one.
pub fn completed_at(
    card: &CardView,
    ends_terminal: bool,
    terminal: impl Fn(&str) -> bool,
) -> Option<u64> {
    if !ends_terminal {
        return None;
    }
    let mut entered = None;
    let mut first = true;
    for row in &card.activity {
        let (from, to) = match &row.kind {
            ActivityKind::Moved { from, to, .. } => (from.as_deref(), to.as_str()),
            ActivityKind::Restored { to, .. } => (None, to.as_str()),
            _ => continue,
        };
        if first && from.is_some_and(&terminal) {
            entered = Some(card.created_at);
        }
        first = false;
        if !terminal(to) {
            entered = None;
        } else if entered.is_none() {
            entered = Some(row.created_at);
        }
    }
    if first {
        return Some(card.created_at);
    }
    // History that ends outside the terminal columns while the card sits in
    // one is history this fold lost a move of: its placement says when it
    // got there.
    Some(entered.unwrap_or(card.placed_at.max(card.created_at)))
}

/// Whether the column a card's history names `column` (by name, else by
/// id) is terminal on `view`.
fn is_terminal(view: &BoardView, column: &str) -> bool {
    view.columns
        .iter()
        .find(|c| c.name == column)
        .or_else(|| view.columns.iter().find(|c| c.id == column))
        .is_some_and(|c| view.column_is_terminal(&c.id))
}

/// Whether `column` is the board's In Review column, matched by id or by
/// name ignoring case, as [`ColumnView::is_in_progress`] matches its own.
fn is_in_review(column: &ColumnView) -> bool {
    column.id == IN_REVIEW_COLUMN || column.name.eq_ignore_ascii_case(IN_REVIEW_NAME)
}

#[cfg(test)]
mod tests {
    use headway::event::ActivityView;
    use nostrdb_net::NoteId;

    use super::*;

    /// A card made at second 100 with `activity`, sitting where its last
    /// move put it, placed at second 900.
    fn card(activity: Vec<ActivityView>) -> CardView {
        CardView {
            id: NoteId::new([1; 32]),
            author: [0; 32],
            title: String::new(),
            description: String::new(),
            labels: Vec::new(),
            priority: headway::event::Priority::None,
            due: None,
            estimate: None,
            rank: String::new(),
            seq: None,
            placed_at: 900,
            created_at: 100,
            updated_at: 100,
            comments: Vec::new(),
            reviews: Vec::new(),
            activity,
            parent: None,
            subissues: Vec::new(),
            blocked_by: Vec::new(),
            blocks: Vec::new(),
            related: Vec::new(),
        }
    }

    /// A move from `from` to `to` at second `at`.
    fn moved(at: u64, from: &str, to: &str) -> ActivityView {
        ActivityView {
            author: [0; 32],
            created_at: at,
            kind: ActivityKind::Moved {
                from: Some(from.to_owned()),
                to: to.to_owned(),
                to_idx: None,
            },
        }
    }

    /// The default board's terminal columns.
    fn terminal(column: &str) -> bool {
        matches!(column, "In Review" | "Done")
    }

    /// A card was finished when it last entered the terminal columns: a
    /// move between two of them doesn't restart the clock, a reopen does,
    /// and a card made in one was finished when it was made.
    #[test]
    fn finished_when_it_last_entered_a_terminal_column() {
        let shipped = card(vec![
            moved(200, "Todo", "In Progress"),
            moved(300, "In Progress", "In Review"),
            moved(400, "In Review", "Done"),
        ]);
        assert_eq!(completed_at(&shipped, true, terminal), Some(300));

        let reopened = card(vec![
            moved(200, "Todo", "Done"),
            moved(300, "Done", "Todo"),
            moved(500, "Todo", "In Review"),
        ]);
        assert_eq!(completed_at(&reopened, true, terminal), Some(500));

        let born_done = card(vec![moved(200, "Done", "In Review")]);
        assert_eq!(completed_at(&born_done, true, terminal), Some(100));
        assert_eq!(completed_at(&card(Vec::new()), true, terminal), Some(100));

        let open = card(vec![moved(200, "Todo", "In Progress")]);
        assert_eq!(completed_at(&open, false, terminal), None);
        // History that lost the move into the terminal column it sits in:
        // its placement dates it.
        assert_eq!(completed_at(&open, true, terminal), Some(900));
    }
}

#[cfg(test)]
mod board_tests {
    use headway::boards::{Identity, Roster};
    use headway::event::{self, BoardView};
    use headway::store::{self, BoardAction, NewCard, NoPublish, Signer};
    use headway::teams::RootRegistry;
    use nostrdb::{Filter, SubscriptionStream};
    use nostrdb_net::FullKeypair;
    use nostrdb_net::NoteId;

    use super::*;
    use crate::test_util::{INGEST_TIMEOUT, TempNdb};

    /// The board the tests write to.
    const SLUG: &str = "work";

    /// The default board's column indexes.
    const TODO: usize = 1;
    const IN_PROGRESS: usize = 2;
    const IN_REVIEW: usize = 3;
    const DONE: usize = 4;

    /// One account writing its own plaintext board.
    struct Writer<'a> {
        ndb: &'a Ndb,
        kp: FullKeypair,
        registry: RootRegistry,
    }

    impl Writer<'_> {
        /// The board, once `ready` says the fold shows what was written.
        /// Subscribed before the first fold, so a note that lands between
        /// the write and the subscription is seen by that fold, and one
        /// after it wakes the next.
        async fn settle(&mut self, ready: impl Fn(&BoardView) -> bool) -> BoardView {
            let kinds = event::HEADWAY_KINDS.iter().map(|k| *k as u64);
            let sub = self
                .ndb
                .subscribe(&[Filter::new().kinds(kinds).build()])
                .unwrap();
            let mut stream = SubscriptionStream::new(self.ndb.clone(), sub);
            loop {
                let roster =
                    Roster::load(self.ndb, Identity::own(self.kp.pubkey), &mut self.registry);
                let view = boards::load_board(self.ndb, &roster, SLUG);
                if let Some(view) = view.filter(|v| ready(v)) {
                    return view;
                }
                stream
                    .wait_for_notes(1, INGEST_TIMEOUT)
                    .await
                    .expect("a note ingested before the deadline");
            }
        }

        /// Apply `action` to the board as it folds now, then wait until
        /// `ready` sees it.
        async fn apply(&mut self, action: BoardAction, ready: impl Fn(&BoardView) -> bool) {
            let view = self.settle(|_| true).await;
            let secret = self.kp.secret_key.secret_bytes();
            let signer = Signer::new(&secret, None);
            let pubkey = self.kp.pubkey;
            let declined = store::apply(
                self.ndb,
                SLUG,
                &view,
                &pubkey,
                &signer,
                action,
                &mut NoPublish,
            );
            assert!(declined.is_none(), "{declined:?}");
            self.settle(ready).await;
        }

        /// Add a card titled `title` to column `col`; returns its id.
        async fn add(&mut self, title: &str, col: usize) -> NoteId {
            let new = NewCard {
                col,
                title: title.to_owned(),
                ..NewCard::default()
            };
            self.apply(BoardAction::AddCard(new), |v| find(v, title).is_some())
                .await;
            find(&self.settle(|_| true).await, title).unwrap()
        }

        /// Move `card` to column `col`.
        async fn move_to(&mut self, card: NoteId, col: usize) {
            let action = BoardAction::MoveCard {
                card,
                to_col: col,
                to_row: 0,
            };
            self.apply(action, |v| {
                v.columns[col].cards.iter().any(|c| c.id == card)
            })
            .await;
        }
    }

    /// The id of the live card titled `title`.
    fn find(view: &BoardView, title: &str) -> Option<NoteId> {
        let mut cards = view.columns.iter().flat_map(|c| c.cards.iter());
        cards.find(|c| c.title == title).map(|c| c.id)
    }

    /// A seeded board's progress: In Progress and In Review counted by
    /// column, and a finished card for every card in a terminal column or
    /// archived from one, dated when it entered the terminal columns.
    #[tokio::test]
    async fn counts_a_seeded_board() {
        let db = TempNdb::new();
        let kp = FullKeypair::generate();
        let secret = kp.secret_key.secret_bytes();
        store::seed_board(&db.ndb, &kp.pubkey, &secret, SLUG, "Work", &mut NoPublish);
        let mut w = Writer {
            ndb: &db.ndb,
            kp,
            registry: RootRegistry::default(),
        };

        // A move in the second a card was made is folded into its creation
        // and leaves no activity row, so the move this test dates comes
        // after one that may not show.
        let shipped = w.add("shipped", TODO).await;
        w.move_to(shipped, IN_PROGRESS).await;
        w.move_to(shipped, IN_REVIEW).await;
        w.move_to(shipped, DONE).await;
        let reopened = w.add("reopened", TODO).await;
        w.move_to(reopened, DONE).await;
        w.move_to(reopened, TODO).await;
        let working = w.add("working", TODO).await;
        w.move_to(working, IN_PROGRESS).await;
        let reviewing = w.add("reviewing", TODO).await;
        w.move_to(reviewing, IN_REVIEW).await;
        w.add("born done", DONE).await;
        let archived = w.add("archived", TODO).await;
        w.move_to(archived, DONE).await;
        w.apply(BoardAction::ArchiveCard { card: archived }, |v| {
            v.archived.iter().any(|a| a.card.id == archived)
        })
        .await;
        let view = w.settle(|_| true).await;

        let got = progress(&view);
        assert_eq!(got.board, SLUG);
        assert_eq!(got.in_progress, 1);
        assert_eq!(got.in_review, 1);
        assert_eq!(got.completed.len(), 4, "{got:?}");
        assert!(got.completed.is_sorted());

        // `shipped` was finished when it reached In Review, before the
        // move to Done that placed it where it is.
        let card = view.card(shipped).unwrap();
        let reached_review = card.activity.iter().find_map(|row| match &row.kind {
            ActivityKind::Moved { to, .. } if to == "In Review" => Some(row.created_at),
            _ => None,
        });
        let finished = completed_at(card, true, |c| is_terminal(&view, c));
        assert_eq!(finished, reached_review);
        assert!(finished.unwrap() < card.placed_at);
        assert!(got.completed.contains(&finished.unwrap()));

        // A board nobody linked contributes nothing.
        let roster = Roster::load(&db.ndb, Identity::own(w.kp.pubkey), &mut w.registry);
        assert!(load(&db.ndb, &roster, &["nope"]).is_empty());
        assert_eq!(load(&db.ndb, &roster, &[SLUG]), [got]);
    }
}
