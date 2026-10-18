//! Writing a goal's progress back to its Headway card.
//!
//! The card is where the goal lives, so it is where the goal's news goes:
//! a short comment naming the numbers when the goal's [`Status`] changes,
//! and on [`Status::Achieved`] a move to the board's done column.
//!
//! It is written to be **idempotent and quiet**. Every comment starts with a
//! marker line, `cashflow goal: <status> (<YYYY-MM>)` ([`Posted`]), which
//! discovery reads back off the card ([`Goal::posted`]). [`plan`] comments
//! only when that (status, month) differs from the newest comment's and
//! hasn't been posted before, so a goal gets one comment when its status
//! changes and at most one a month as a digest while it doesn't, and a
//! rerun writes nothing. A card already in a done column is left alone.
//!
//! [`write`] carries a plan out through [`headway::store::apply`], signed by
//! the account and sealed into the board's channel when it has one, against
//! a fresh fold of the board, so a comment posted since the plan was made
//! isn't posted twice. The events are ingested into the local nostrdb and
//! handed to the caller's [`Publisher`]: the app passes
//! [`NoPublish`](headway::store::NoPublish) and lets the host fan sealed
//! boards' envelopes out, like the Headway app does.

use headway::boards::{self, Identity, Roster};
use headway::event::{BoardView, CommentView};
use headway::store::{self, BoardAction, Publisher, Signer};
use headway::teams::RootRegistry;
use ledgerdb::{Amount, Commodities, Date, Ymd};
use nostrdb::Ndb;
use nostrdb_net::{NoteId, Pubkey};

use super::progress::{Progress, Status, Unit};
use super::{Direction, Goal, Period};

/// How a write-back comment starts, so it can be found again.
pub const MARKER: &str = "cashflow goal:";

/// What a write-back comment reported: the goal's status, for a month.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Posted {
    /// The status it reported.
    pub status: Status,
    /// The month it was posted in, as [`Date::month_index`].
    pub month: u32,
}

impl Posted {
    /// The comment's marker line: `cashflow goal: on track (2026-10)`.
    pub fn marker(self) -> String {
        let Ymd { year, month, .. } = Date::first_of_month(self.month).ymd();
        format!("{MARKER} {} ({year:04}-{month:02})", self.status.name())
    }

    /// The report a comment's first line is the marker of, if it is one.
    pub fn parse(body: &str) -> Option<Self> {
        let line = body.lines().next()?.trim();
        let rest = line.strip_prefix(MARKER)?.trim_start();
        let (status, month) = rest.strip_suffix(')')?.rsplit_once(" (")?;
        let status = Status::from_name(status)?;
        let month = Date::parse(&format!("{month}-01")).ok()?.month_index();
        Some(Self { status, month })
    }
}

/// The write-back reports among `comments`, in their order.
pub fn posted(comments: &[CommentView]) -> Vec<Posted> {
    comments
        .iter()
        .filter_map(|c| Posted::parse(&c.body))
        .collect()
}

/// What to write to one goal's card.
#[derive(Debug, Clone, PartialEq)]
pub struct Write {
    /// The card, as a full `headway:<board>/<word-id>` ref.
    pub card_ref: String,
    /// The board's slug.
    pub board: String,
    /// The board's owner.
    pub owner: [u8; 32],
    /// The card's id.
    pub card: [u8; 32],
    /// The report the comment makes.
    pub posted: Posted,
    /// The comment to post, marker line first, or `None` when this report
    /// is already on the card.
    pub comment: Option<String>,
    /// Whether to move the card to the board's done column.
    pub done: bool,
}

/// What, if anything, to write to `goal`'s card now that it measures
/// `progress` on `today`. `auto_done` moves an achieved goal's card to done
/// (`[goals] auto_done`). `commodities` writes the amounts.
///
/// Nothing for a card already in a done column. A comment when the newest
/// report on the card isn't this (status, month) and this one was never
/// posted; a move when the goal is achieved.
pub fn plan(
    goal: &Goal,
    progress: &Progress,
    today: Date,
    auto_done: bool,
    commodities: &Commodities,
) -> Option<Write> {
    if goal.done {
        return None;
    }
    let posted = Posted {
        status: progress.status,
        month: today.month_index(),
    };
    let fresh = goal.posted.last() != Some(&posted) && !goal.posted.contains(&posted);
    let comment = fresh.then(|| comment(posted, goal, progress, commodities));
    let done = auto_done && progress.status == Status::Achieved;
    if comment.is_none() && !done {
        return None;
    }
    Some(Write {
        card_ref: goal.card_ref.clone(),
        board: goal.board.clone(),
        owner: goal.owner,
        card: goal.card,
        posted,
        comment,
        done,
    })
}

/// The comment for `posted`: the marker, then the numbers in a line.
/// `current of target · pace · when`.
fn comment(posted: Posted, goal: &Goal, progress: &Progress, commodities: &Commodities) -> String {
    let text = |value: f64| match progress.unit {
        Unit::Amount(commodity) => Amount {
            qty: value.round() as i64,
            commodity,
        }
        .display(commodities)
        .to_string(),
        Unit::Percent => format!("{value:.1}%"),
    };
    let monthly = match progress.unit {
        Unit::Amount(_) => "/mo",
        Unit::Percent => "",
    };
    let per = match goal.spec.as_ref().map(|spec| spec.per) {
        Ok(Period::Month) => " this month",
        Ok(Period::Year) => " this year",
        _ => "",
    };
    let mut line = format!(
        "{} of {}{per}",
        text(progress.current),
        text(progress.target)
    );
    let achieved = progress.status == Status::Achieved;
    if let Some(required) = progress.pace.required.filter(|_| !achieved) {
        let what = match progress.direction {
            Direction::AtLeast => "needs",
            Direction::AtMost => "room for",
        };
        line += &format!(" · {what} {}{monthly}", text(required.max(0.0)));
    }
    if !achieved && !progress.closed {
        line += &format!(" · trending {}{monthly}", text(progress.pace.projected));
    }
    let monthly_goal = matches!(goal.spec, Ok(ref spec) if spec.per == Period::Month);
    match (progress.direction, achieved, progress.eta) {
        (Direction::AtMost, ..) | (_, true, _) => {}
        (_, _, Some(eta)) if monthly_goal => {
            let Ymd { year, month, .. } = eta.ymd();
            line += &format!(" · ETA {year:04}-{month:02}");
        }
        (_, _, Some(eta)) => line += &format!(" · ETA {eta}"),
        (_, _, None) if progress.closed => line += " · closed short of it",
        (_, _, None) => line += " · no ETA at this pace",
    }
    format!("{}\n\n{line}", posted.marker())
}

/// What [`write`] did to one card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Written {
    /// Whether it posted the comment.
    pub commented: bool,
    /// Whether it moved the card to done.
    pub moved: bool,
}

/// Why [`write`] couldn't write to a card.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    /// The board isn't in this nostrdb.
    #[error("board `{0}` isn't here")]
    NoBoard(String),
    /// The card isn't on its board any more.
    #[error("{0} isn't on its board")]
    NoCard(String),
}

/// Carry `writes` out as `me`, signing with `secret`: each card's comment,
/// then its move to done, applied to a fresh fold of its board. A comment
/// whose report the card already carries is skipped, and so is a move of a
/// card already in a done column, so a write repeated before the first
/// one's notes fold in is a no-op on the board. `registry` registers each
/// board owner's sealed roots, like discovery's.
pub fn write(
    ndb: &Ndb,
    me: Pubkey,
    secret: &[u8; 32],
    registry: &mut RootRegistry,
    writes: &[Write],
    publisher: &mut dyn Publisher,
) -> Vec<Result<Written, WriteError>> {
    writes
        .iter()
        .map(|w| write_one(ndb, me, secret, registry, w, publisher))
        .collect()
}

/// [`write`] for one card.
fn write_one(
    ndb: &Ndb,
    me: Pubkey,
    secret: &[u8; 32],
    registry: &mut RootRegistry,
    write: &Write,
    publisher: &mut dyn Publisher,
) -> Result<Written, WriteError> {
    let owner = Pubkey::new(write.owner);
    let roster = Roster::load(ndb, Identity { me, owner }, registry);
    let view = boards::load_board(ndb, &roster, &write.board)
        .ok_or_else(|| WriteError::NoBoard(write.board.clone()))?;
    let card = NoteId::new(write.card);
    let found = view
        .card(card)
        .ok_or_else(|| WriteError::NoCard(write.card_ref.clone()))?;
    let channel = roster.channel(&write.board);
    let signer = Signer::new(secret, channel.as_ref());
    let mut apply =
        |action| store::apply(ndb, &write.board, &view, &owner, &signer, action, publisher);

    let mut written = Written::default();
    if let Some(body) = &write.comment
        && !posted(&found.comments).contains(&write.posted)
    {
        apply(BoardAction::AddComment {
            card,
            body: body.clone(),
            reply_to: None,
        });
        written.commented = true;
    }
    if write.done
        && !view.card_is_done(card)
        && let Some(to_col) = done_column(&view)
    {
        apply(BoardAction::MoveCard {
            card,
            to_col,
            to_row: view.columns[to_col].cards.len(),
        });
        written.moved = true;
    }
    Ok(written)
}

/// The board's done column: its last terminal one (with In Review and Done
/// both terminal, Done).
fn done_column(view: &BoardView) -> Option<usize> {
    view.columns
        .iter()
        .rposition(|col| view.column_is_terminal(&col.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goals::progress::{self, Inputs};
    use crate::goals::{FENCE_INFO, parse_goal};
    use crate::projects::Projects;
    use crate::test_util::books;

    fn date(text: &str) -> Date {
        Date::parse(text).unwrap()
    }

    /// A goal of earning 1000 CAD a month, on a card carrying `posted`.
    fn goal(posted: &[Posted]) -> Goal {
        let desc = format!("```{FENCE_INFO}\nmetric: income\ntarget: 1000 CAD\n```\n");
        Goal {
            card_ref: "headway:cashflow/a-b-c".to_owned(),
            board: "cashflow".to_owned(),
            owner: [1; 32],
            card: [2; 32],
            title: "Earn".to_owned(),
            column: "Todo".to_owned(),
            done: false,
            done_at: None,
            spec: parse_goal(&desc).unwrap(),
            posted: posted.to_vec(),
        }
    }

    /// The books of `income` CAD earned on 2024-06-01, and the goal measured
    /// on `today`.
    fn measured(income: &str, today: &str) -> (crate::Books, Progress) {
        let books = books(&format!(
            "2024-06-01 * t\n  income:salary  -{income} CAD\n  assets:checking\n\n"
        ));
        let spec = goal(&[]).spec.unwrap();
        let progress = progress::evaluate(
            &Inputs {
                books: &books,
                projects: &Projects::default(),
                today: date(today),
            },
            &spec,
        )
        .unwrap();
        (books, progress)
    }

    fn plan_for(goal: &Goal, income: &str, today: &str, auto_done: bool) -> Option<Write> {
        let (books, progress) = measured(income, today);
        let commodities = &books.ledger().journal().commodities;
        plan(goal, &progress, date(today), auto_done, commodities)
    }

    fn june(status: Status) -> Posted {
        Posted {
            status,
            month: date("2024-06-01").month_index(),
        }
    }

    /// The marker line reads back to the report it was written from, and
    /// anything else isn't one.
    #[test]
    fn marker_round_trips() {
        for status in [
            Status::Achieved,
            Status::OnTrack,
            Status::AtRisk,
            Status::Behind,
        ] {
            let posted = june(status);
            let body = format!("{}\n\nnumbers", posted.marker());
            assert_eq!(Posted::parse(&body), Some(posted), "{body}");
        }
        assert_eq!(
            june(Status::AtRisk).marker(),
            "cashflow goal: at risk (2024-06)"
        );
        assert_eq!(Posted::parse("looks good"), None);
        assert_eq!(Posted::parse("cashflow goal: winning (2024-06)"), None);
        assert_eq!(Posted::parse("cashflow goal: behind (June)"), None);
    }

    /// A goal with no report yet gets one comment naming the numbers, and
    /// no move short of achieved.
    #[test]
    fn first_status_comments() {
        let write = plan_for(&goal(&[]), "450.00", "2024-06-15", true).unwrap();
        assert_eq!(write.posted, june(Status::AtRisk));
        assert!(!write.done);
        assert_eq!(
            write.comment.as_deref(),
            Some(
                "cashflow goal: at risk (2024-06)\n\n\
                 450.00 CAD of 1000.00 CAD this month · needs 1100.00 CAD/mo · \
                 trending 900.00 CAD/mo · no ETA at this pace"
            )
        );
    }

    /// A status change comments; the same status the same month doesn't.
    #[test]
    fn comments_only_on_change() {
        let changed = plan_for(
            &goal(&[june(Status::OnTrack)]),
            "450.00",
            "2024-06-15",
            true,
        );
        assert_eq!(changed.unwrap().posted, june(Status::AtRisk));
        let same = plan_for(&goal(&[june(Status::AtRisk)]), "450.00", "2024-06-15", true);
        assert_eq!(same, None);
    }

    /// Flapping back to a status already reported this month stays quiet.
    #[test]
    fn never_repeats_a_months_status() {
        let posted = [june(Status::AtRisk), june(Status::OnTrack)];
        assert_eq!(plan_for(&goal(&posted), "450.00", "2024-06-15", true), None);
    }

    /// An unchanged status gets one digest in a new month.
    #[test]
    fn monthly_digest() {
        let may = Posted {
            status: Status::Behind,
            month: date("2024-05-01").month_index(),
        };
        let write = plan_for(&goal(&[may]), "300.00", "2024-06-15", true).unwrap();
        assert_eq!(write.posted, june(Status::Behind));
        assert!(write.comment.is_some());
    }

    /// Achieved comments and moves to done; once commented, a pending move
    /// still goes; `auto_done` off only comments; a done card gets nothing.
    #[test]
    fn achieved_moves_to_done() {
        let write = plan_for(&goal(&[]), "1200.00", "2024-06-15", true).unwrap();
        assert!(write.done);
        let comment = write.comment.unwrap();
        assert!(
            comment.starts_with("cashflow goal: achieved (2024-06)\n\n1200.00 CAD of 1000.00 CAD"),
            "{comment}"
        );
        assert!(!comment.contains("ETA"), "{comment}");

        let commented = goal(&[june(Status::Achieved)]);
        let write = plan_for(&commented, "1200.00", "2024-06-15", true).unwrap();
        assert_eq!((write.comment, write.done), (None, true));

        let manual = plan_for(&goal(&[]), "1200.00", "2024-06-15", false).unwrap();
        assert!(manual.comment.is_some() && !manual.done);
        assert_eq!(plan_for(&commented, "1200.00", "2024-06-15", false), None);

        let done = Goal {
            done: true,
            ..goal(&[])
        };
        assert_eq!(plan_for(&done, "1200.00", "2024-06-15", true), None);
    }
}

#[cfg(test)]
mod board_tests {
    use headway::event;
    use headway::store::{NewCard, NoPublish};
    use nostrdb::{Filter, SubscriptionStream};
    use nostrdb_net::FullKeypair;

    use super::*;
    use crate::goals::{GOAL_LABEL, discover};
    use crate::test_util::{INGEST_TIMEOUT, TempNdb};

    /// Re-run `check` after each ingested headway note until it returns
    /// something. See `discovery_tests::settle`.
    async fn settle<T>(ndb: &Ndb, mut check: impl FnMut() -> Option<T>) -> T {
        let kinds = event::HEADWAY_KINDS
            .iter()
            .map(|k| *k as u64)
            .chain([u64::from(nostrdb_net::sns::KEYSHARE_KIND)]);
        let sub = ndb
            .subscribe(&[Filter::new().kinds(kinds).build()])
            .unwrap();
        let mut stream = SubscriptionStream::new(ndb.clone(), sub);
        loop {
            if let Some(found) = check() {
                return found;
            }
            stream
                .wait_for_notes(1, INGEST_TIMEOUT)
                .await
                .expect("a note ingested before the deadline");
        }
    }

    /// Counts the frames handed to it.
    #[derive(Default)]
    struct Count(usize);

    impl Publisher for Count {
        fn publish(&mut self, _frame: &str) {
            self.0 += 1;
        }
    }

    /// The one goal on `kp`'s sealed `cashflow` board once `ready` holds.
    async fn the_goal(
        ndb: &Ndb,
        kp: &FullKeypair,
        registry: &mut RootRegistry,
        ready: impl Fn(&Goal) -> bool,
    ) -> Goal {
        settle(ndb, || {
            let roster = Roster::load(ndb, Identity::own(kp.pubkey), registry);
            discover(ndb, &roster, &["cashflow"])
                .into_iter()
                .find(|g| ready(g))
        })
        .await
    }

    /// On a sealed board: a status comment lands on the card and reads back
    /// as its report; achieved comments and moves the card to Done; a rerun
    /// of the same writes is a no-op.
    #[tokio::test]
    async fn writes_comment_and_moves_to_done() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let me = FullKeypair::generate();
        let secret = me.secret_key.secret_bytes();
        ndb.add_key(&secret);
        let mut registry = RootRegistry::default();
        let root = nostrdb_net::sns::derive_board_root(&secret, "cashflow");
        assert!(store::create_shared_board(
            ndb,
            &me.pubkey,
            &secret,
            "cashflow",
            "Cashflow",
            &root,
            &mut NoPublish,
        ));
        let view = settle(ndb, || {
            let roster = Roster::load(ndb, Identity::own(me.pubkey), &mut registry);
            boards::load_board(ndb, &roster, "cashflow")
        })
        .await;
        let roster = Roster::load(ndb, Identity::own(me.pubkey), &mut registry);
        let channel = roster.channel("cashflow").expect("born sealed");
        store::apply(
            ndb,
            "cashflow",
            &view,
            &me.pubkey,
            &Signer::shared(&secret, &channel),
            BoardAction::AddCard(NewCard {
                col: 0,
                title: "Earn".to_owned(),
                description: "```cashflow-goal\nmetric: income\ntarget: 1000 CAD\n```\n".to_owned(),
                labels: vec![GOAL_LABEL.to_owned()],
                ..NewCard::default()
            }),
            &mut NoPublish,
        );
        let goal = the_goal(ndb, &me, &mut registry, |_| true).await;
        assert!(goal.posted.is_empty());

        // A status comment.
        let month = Date::parse("2024-06-01").unwrap().month_index();
        let at_risk = Write {
            posted: Posted {
                status: Status::AtRisk,
                month,
            },
            comment: Some("cashflow goal: at risk (2024-06)\n\nnumbers".to_owned()),
            ..write_for(&goal)
        };
        let mut sink = Count::default();
        let out = write(
            ndb,
            me.pubkey,
            &secret,
            &mut registry,
            std::slice::from_ref(&at_risk),
            &mut sink,
        );
        assert_eq!(
            out,
            [Ok(Written {
                commented: true,
                moved: false
            })]
        );
        assert_eq!(sink.0, 1, "one event handed to the publisher");
        let goal = the_goal(ndb, &me, &mut registry, |g| !g.posted.is_empty()).await;
        assert_eq!(goal.posted, [at_risk.posted]);

        // Rerun: the report is on the card, so nothing.
        let out = write(
            ndb,
            me.pubkey,
            &secret,
            &mut registry,
            &[at_risk],
            &mut sink,
        );
        assert_eq!(out, [Ok(Written::default())]);
        assert_eq!(sink.0, 1);

        // Achieved: comment, and move to the board's done column.
        let achieved = Write {
            posted: Posted {
                status: Status::Achieved,
                month,
            },
            comment: Some("cashflow goal: achieved (2024-06)\n\nnumbers".to_owned()),
            done: true,
            ..write_for(&goal)
        };
        let out = write(
            ndb,
            me.pubkey,
            &secret,
            &mut registry,
            std::slice::from_ref(&achieved),
            &mut sink,
        );
        assert_eq!(
            out,
            [Ok(Written {
                commented: true,
                moved: true
            })]
        );
        let goal = the_goal(ndb, &me, &mut registry, |g| g.done && g.posted.len() == 2).await;
        assert_eq!(goal.column, "Done");
        assert_eq!(goal.posted.last(), Some(&achieved.posted));

        // Rerun once folded: a no-op.
        let out = write(
            ndb,
            me.pubkey,
            &secret,
            &mut registry,
            &[achieved],
            &mut sink,
        );
        assert_eq!(out, [Ok(Written::default())]);
        assert_eq!(sink.0, 3);
    }

    /// A write addressed at `goal`'s card, saying nothing.
    fn write_for(goal: &Goal) -> Write {
        Write {
            card_ref: goal.card_ref.clone(),
            board: goal.board.clone(),
            owner: goal.owner,
            card: goal.card,
            posted: Posted {
                status: Status::OnTrack,
                month: 0,
            },
            comment: None,
            done: false,
        }
    }
}
