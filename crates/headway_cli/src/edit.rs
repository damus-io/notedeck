//! The edit path: turning a parsed [`Command`] into a [`BoardAction`] by
//! resolving its card, column, container and comment arguments against the
//! folded board, and the [`Collect`] sink that gathers the frames an edit
//! produces.

use nostrdb_net::NoteId;

use headway::event::{
    self, BoardView, CardView, Container, Date, LineSide, Priority, ReviewCommentView,
    ReviewLocation, ReviewView, resolve_card,
};
use headway::store::{self, BoardAction, NewReviewComment, Publisher};
use headway::wordid;

use nostrdb_net::relay::sync::Result;

use crate::args::{Command, ReviewCommentFlags, SeqSpec};
use crate::diff;

/// Translate a resolved [`Command`] into a [`BoardAction`], resolving card and
/// column arguments against `view`.
pub(crate) fn build_action(view: &BoardView, command: Command) -> Result<BoardAction> {
    Ok(match command {
        Command::Add {
            title,
            col,
            labels,
            parent,
            description,
        } => {
            let col = col.as_deref().map_or(Ok(0), |c| resolve_col(view, c))?;
            let parent = parent
                .as_deref()
                .map(|sel| resolve_card(view, sel))
                .transpose()?;
            BoardAction::AddCard {
                col,
                title,
                description: description.unwrap_or_default(),
                labels,
                parent,
            }
        }
        Command::Move { card, col, row } => {
            let card = resolve_card(view, &card)?;
            let to_col = resolve_col(view, &col)?;
            let to_row = row.unwrap_or(view.columns[to_col].cards.len());
            BoardAction::MoveCard {
                card,
                to_col,
                to_row,
            }
        }
        Command::Title { card, title } => BoardAction::EditTitle {
            card: resolve_card(view, &card)?,
            title,
        },
        Command::Desc { card, text } => BoardAction::EditDescription {
            card: resolve_card(view, &card)?,
            description: text,
        },
        Command::Label { card, labels } => BoardAction::SetLabels {
            card: resolve_card(view, &card)?,
            labels,
        },
        Command::Priority { card, level } => BoardAction::SetPriority {
            card: resolve_card(view, &card)?,
            priority: Priority::parse(&level),
        },
        Command::Due { card, date } => BoardAction::SetDue {
            card: resolve_card(view, &card)?,
            due: parse_clearable(&date, |s| {
                Date::parse(s).ok_or_else(|| format!("invalid date '{s}' (want YYYY-MM-DD)").into())
            })?,
        },
        Command::Estimate { card, points } => BoardAction::SetEstimate {
            card: resolve_card(view, &card)?,
            estimate: parse_clearable(&points, |s| {
                s.parse::<u32>()
                    .map_err(|_| format!("invalid estimate '{s}' (want a number)").into())
            })?,
        },
        Command::Parent { card, parent } => BoardAction::SetParent {
            card: resolve_card(view, &card)?,
            parent: parent
                .as_deref()
                .map(|sel| resolve_card(view, sel))
                .transpose()?,
        },
        Command::Block { card, on } => BoardAction::Block {
            card: resolve_card(view, &card)?,
            on: resolve_card(view, &on)?,
        },
        Command::Unblock { card, on } => BoardAction::Unblock {
            card: resolve_card(view, &card)?,
            on: resolve_card(view, &on)?,
        },
        Command::Relate { card, other } => BoardAction::Relate {
            card: resolve_card(view, &card)?,
            other: resolve_card(view, &other)?,
        },
        Command::Unrelate { card, other } => BoardAction::Unrelate {
            card: resolve_card(view, &card)?,
            other: resolve_card(view, &other)?,
        },
        Command::Seq {
            card,
            spec,
            container,
        } => {
            let card = resolve_card(view, &card)?;
            let container = match container.as_deref() {
                Some(sel) => resolve_container(view, sel)?,
                None => Container::BoardRoot(view.id.clone()),
            };
            let position = match spec {
                SeqSpec::First => store::SeqPosition::First,
                SeqSpec::Last => store::SeqPosition::Last,
                SeqSpec::After(a) => store::SeqPosition::After(resolve_card(view, &a)?),
                SeqSpec::Before(a) => store::SeqPosition::Before(resolve_card(view, &a)?),
            };
            let rank = store::seq_rank(view, &container, card, &position)?;
            BoardAction::SetSequence {
                card,
                container,
                rank,
            }
        }
        Command::Comment {
            card,
            body,
            reply_to,
            review,
        } => comment_action(
            view,
            resolve_card(view, &card)?,
            body,
            reply_to.as_deref(),
            &review,
        )?,
        Command::Review { card, review } => BoardAction::AddReview {
            card: resolve_card(view, &card)?,
            review,
        },
        Command::Delete { card } => BoardAction::DeleteCard {
            card: resolve_card(view, &card)?,
        },
        Command::Archive { card } => BoardAction::ArchiveCard {
            card: resolve_card(view, &card)?,
        },
        Command::Restore { card } => BoardAction::RestoreCard {
            card: resolve_card(view, &card)?,
        },
        Command::Rename { title } => BoardAction::RenameBoard { title },
        Command::Terminal { col, terminal } => BoardAction::SetColumnTerminal {
            col: resolve_col(view, &col)?,
            terminal,
        },
        Command::Show { .. }
        | Command::Next { .. }
        | Command::Grep { .. }
        | Command::Diff { .. }
        | Command::Seed { .. }
        | Command::Migrate
        | Command::Share { .. }
        | Command::Link { .. }
        | Command::MoveBoard { .. }
        | Command::Board { .. }
        | Command::Login { .. }
        | Command::Logout => {
            unreachable!("handled before build_action")
        }
    })
}

/// Parse a "set or clear" scalar CLI value: `none` (or an empty string) clears
/// the field (`Ok(None)`); any other value is run through `parse`. Shared by the
/// `due`/`estimate` commands so both take `none` to clear.
fn parse_clearable<T>(s: &str, parse: impl Fn(&str) -> Result<T>) -> Result<Option<T>> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    parse(s).map(Some)
}

/// Collects the `["EVENT", {...}]` frames an edit produces so they can be
/// forwarded to the relay after `apply` returns.
#[derive(Default)]
pub(crate) struct Collect(pub(crate) Vec<String>);

impl Publisher for Collect {
    fn publish(&mut self, frame: &str) {
        self.0.push(frame.to_string());
    }
}

fn resolve_col(view: &BoardView, sel: &str) -> Result<usize> {
    view.columns
        .iter()
        .position(|c| c.id == sel || c.name.eq_ignore_ascii_case(sel))
        .ok_or_else(|| {
            let names: Vec<&str> = view.columns.iter().map(|c| c.name.as_str()).collect();
            format!("no column matching '{sel}'; columns: {}", names.join(", ")).into()
        })
}

/// Resolve an `--in` container selector: a card ref names that card's container
/// (its subissues); anything else must be the board's own slug, naming the
/// board-root work-order.
pub(crate) fn resolve_container(view: &BoardView, sel: &str) -> Result<Container> {
    match resolve_card(view, sel) {
        Ok(id) => Ok(Container::Card(*id.bytes())),
        Err(_) if sel.eq_ignore_ascii_case(&view.id) => Ok(Container::BoardRoot(view.id.clone())),
        Err(e) => Err(e.into()),
    }
}

/// The action `comment` posts on `card`, picked by what it answers:
///
/// - `--reply-to` a review comment: a review comment on that comment's record,
///   under the same lines, so it shows beside it in the app's diff.
/// - `--reply-to` a card comment, or no flags: a comment in the card's thread.
/// - `--path`/`--line`/`--old`/`--record`: a new inline comment on a review
///   record (the newest, or the one `--record` picks), on the whole commit
///   when no lines are given.
///
/// A reply takes its record and lines from the comment it answers, so the
/// review flags are refused beside it rather than silently ignored.
fn comment_action(
    view: &BoardView,
    card: NoteId,
    body: String,
    reply_to: Option<&str>,
    flags: &ReviewCommentFlags,
) -> Result<BoardAction> {
    let found = event::all_cards(view)
        .find(|c| c.id == card)
        .ok_or("no such card")?;
    let target = reply_to
        .map(|sel| resolve_comment(found, sel))
        .transpose()?;
    if target.is_some() && flags.any() {
        return Err(
            "--reply-to threads under the comment it names, on its record and \
                    lines; drop --path/--line/--old/--record"
                .into(),
        );
    }
    let (record, comment) = match target {
        Some(CommentTarget::Review { record, comment }) => (
            record,
            NewReviewComment {
                location: comment.location.clone(),
                body,
                reply_to: Some(comment.id),
            },
        ),
        Some(CommentTarget::Card(id)) => {
            return Ok(BoardAction::AddComment {
                card,
                body,
                reply_to: Some(id),
            });
        }
        None if !flags.any() => {
            return Ok(BoardAction::AddComment {
                card,
                body,
                reply_to: None,
            });
        }
        None => {
            let record = diff::pick_record(&found.reviews, flags.record.as_deref())?.ok_or(
                "this card has no review record to comment on (`headway review` adds one)",
            )?;
            let location = review_location(record, flags)?;
            (
                record,
                NewReviewComment {
                    location,
                    body,
                    reply_to: None,
                },
            )
        }
    };
    Ok(BoardAction::AddReviewComments {
        card,
        record: record.id,
        comments: vec![comment],
    })
}

/// Where on `record`'s commit a new inline comment points, from `--path`,
/// `--line` and `--old`: `None` (the whole commit) when none of them is given.
/// A path needs lines and lines need a path, and the record must name its
/// commit, since the location carries it.
fn review_location(
    record: &ReviewView,
    flags: &ReviewCommentFlags,
) -> Result<Option<ReviewLocation>> {
    let (path, (start, end)) = match (&flags.path, flags.lines) {
        (None, None) if flags.old => return Err("--old needs --path and --line".into()),
        (None, None) => return Ok(None),
        (Some(path), Some(lines)) => (path, lines),
        (Some(_), None) => return Err("--path needs --line <a[-b]>".into()),
        (None, Some(_)) => return Err("--line needs --path <file>".into()),
    };
    let commit = record
        .fields
        .commit
        .clone()
        .ok_or("that review record names no commit to put lines on")?;
    Ok(Some(ReviewLocation {
        path: path.clone(),
        commit,
        start,
        end,
        side: if flags.old {
            LineSide::Old
        } else {
            LineSide::New
        },
    }))
}

/// What a `--reply-to` selector named on a card: a comment in its own thread,
/// or a review comment on one of its records.
enum CommentTarget<'a> {
    Card(NoteId),
    Review {
        record: &'a ReviewView,
        comment: &'a ReviewCommentView,
    },
}

/// Resolve a `--reply-to` selector against `card`'s comments and the review
/// comments on its records, accepting a full hex id, a unique hex prefix, or a
/// comment word-id. Comments render as a bare word-id in `show` (not a
/// `headway:<board>/…` card ref), so the bare word-id is the selector here. A
/// full hex id found in neither is taken as a card comment that hasn't folded
/// yet, as it always was.
fn resolve_comment<'a>(card: &'a CardView, sel: &str) -> Result<CommentTarget<'a>> {
    let review = || {
        card.reviews.iter().flat_map(|record| {
            record
                .comments
                .iter()
                .map(move |comment| (comment.id, CommentTarget::Review { record, comment }))
        })
    };
    let all = || {
        card.comments
            .iter()
            .map(|c| (c.id, CommentTarget::Card(c.id)))
            .chain(review())
    };

    if let Ok(id) = NoteId::from_hex(sel) {
        return Ok(all()
            .find(|(cid, _)| *cid == id)
            .map_or(CommentTarget::Card(id), |(_, t)| t));
    }
    let sel = sel.to_lowercase();
    if let Some((_, t)) = all().find(|(id, _)| wordid::encode(id.bytes()) == sel) {
        return Ok(t);
    }

    let mut hits = all().filter(|(id, _)| id.hex().starts_with(&sel));
    match (hits.next(), hits.next()) {
        (Some((_, t)), None) => Ok(t),
        (Some(_), Some(_)) => Err(format!("ambiguous comment prefix '{sel}'").into()),
        _ => Err(format!("no comment matching '{sel}' on this card").into()),
    }
}
