//! Reading an edit's arguments against a folded board: the column and
//! review-record pickers, the `due`/`estimate`/`--line` value parsers, and
//! [`comment_action`], which picks what a comment answers. Shared by the
//! `headway` CLI and the app's agent tools (`notedeck_headway`), so an edit
//! typed into either resolves the same way and fails with the same words.

use nostrdb_net::NoteId;

use crate::event::{
    self, BoardView, CardView, Container, Date, LineRange, LineSide, ReviewCommentView,
    ReviewLocation, ReviewView,
};
use crate::store::{BoardAction, NewReviewComment};
use crate::wordid;

/// Resolve a column selector, its id or its name in any case, to its index on
/// `view`. A miss names the board's columns.
pub fn resolve_col(view: &BoardView, sel: &str) -> Result<usize, String> {
    view.columns
        .iter()
        .position(|c| c.id == sel || c.name.eq_ignore_ascii_case(sel))
        .ok_or_else(|| {
            let names: Vec<&str> = view.columns.iter().map(|c| c.name.as_str()).collect();
            format!("no column matching '{sel}'; columns: {}", names.join(", "))
        })
}

/// Resolve an `--in` container selector: a card ref names that card's
/// container (its subissues); anything else must be the board's own slug,
/// naming the board-root work-order. `next`, `seq` and `grep` take it.
pub fn resolve_container(view: &BoardView, sel: &str) -> Result<Container, String> {
    match event::resolve_card(view, sel) {
        Ok(id) => Ok(Container::Card(*id.bytes())),
        Err(_) if sel.eq_ignore_ascii_case(&view.id) => Ok(Container::BoardRoot(view.id.clone())),
        Err(e) => Err(e),
    }
}

/// [`resolve_container`] for an optional `--in`: the board root when it is
/// left out.
pub fn container_or_root(view: &BoardView, sel: Option<&str>) -> Result<Container, String> {
    match sel {
        Some(sel) => resolve_container(view, sel),
        None => Ok(Container::BoardRoot(view.id.clone())),
    }
}

/// A due date as `due` and `add --due` take it: `YYYY-MM-DD`, or `none` (or
/// nothing) to clear it.
pub fn parse_due(s: &str) -> Result<Option<Date>, String> {
    parse_clearable(s, |s| {
        Date::parse(s).ok_or_else(|| format!("invalid date '{s}' (want YYYY-MM-DD)"))
    })
}

/// An estimate as `estimate` and `add --estimate` take it: a number, or
/// `none` (or nothing) to clear it.
pub fn parse_estimate(s: &str) -> Result<Option<u32>, String> {
    parse_clearable(s, |s| {
        s.parse::<u32>()
            .map_err(|_| format!("invalid estimate '{s}' (want a number)"))
    })
}

/// A "set or clear" value: `none` or an empty string clears the field
/// (`Ok(None)`); anything else goes through `parse`.
fn parse_clearable<T>(
    s: &str,
    parse: impl Fn(&str) -> Result<T, String>,
) -> Result<Option<T>, String> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    parse(s).map(Some)
}

/// Parse a `--line` value: `42` for one line or `42-48` for a range, 1-based
/// and inclusive, so `0` and a backwards range are refused.
pub fn parse_lines(value: &str) -> Result<LineRange, String> {
    let bad = || format!("--line wants <a> or <a-b> (1-based), got '{value}'");
    let num = |s: &str| s.trim().parse::<u32>().ok().filter(|&n| n > 0);
    let (start, end) = match value.split_once('-') {
        Some((a, b)) => (num(a).ok_or_else(bad)?, num(b).ok_or_else(bad)?),
        None => {
            let a = num(value).ok_or_else(bad)?;
            (a, a)
        }
    };
    if end < start {
        return Err(bad());
    }
    Ok(LineRange { start, end })
}

/// The review record a command picks on a card: the newest (`reviews` is
/// newest first) when `prefix` is `None`, else the first whose commit starts
/// with it. `Ok(None)` only for a card with no records and no prefix; a
/// prefix that matches nothing is an error, never a fallback to the newest.
pub fn pick_record<'a>(
    reviews: &'a [ReviewView],
    prefix: Option<&str>,
) -> Result<Option<&'a ReviewView>, String> {
    let Some(prefix) = prefix else {
        return Ok(reviews.first());
    };
    reviews
        .iter()
        .find(|r| {
            r.fields
                .commit
                .as_deref()
                .is_some_and(|c| c.starts_with(prefix))
        })
        .map(Some)
        .ok_or_else(|| format!("no review record with a commit starting '{prefix}'"))
}

/// Where on which review record a new inline comment goes, as `comment`'s
/// `--path`, `--line`, `--old` and `--record` give it. All unset means an
/// ordinary card comment.
#[derive(Debug, Default)]
pub struct ReviewCommentFlags {
    /// `--path`: the file, as the diff names it.
    pub path: Option<String>,
    /// `--line`: one line or an inclusive range (see [`parse_lines`]).
    pub lines: Option<LineRange>,
    /// `--old`: the lines are on the old (deleted) side.
    pub old: bool,
    /// `--record`: the record whose commit starts with this prefix, rather
    /// than the newest.
    pub record: Option<String>,
}

impl ReviewCommentFlags {
    /// Any review flag was given, so the comment goes on a record.
    pub fn any(&self) -> bool {
        self.path.is_some() || self.lines.is_some() || self.old || self.record.is_some()
    }
}

/// The action `comment` posts on `card`, picked by what it answers:
///
/// - `reply_to` a review comment: a review comment on that comment's record,
///   under the same lines, so it shows beside it in the app's diff.
/// - `reply_to` a card comment, or no flags: a comment in the card's thread.
/// - `flags` set: a new inline comment on a review record (the newest, or the
///   one `--record` picks), on the whole commit when no lines are given.
///
/// A reply takes its record and lines from the comment it answers, so the
/// review flags are refused beside it rather than silently ignored.
pub fn comment_action(
    view: &BoardView,
    card: NoteId,
    body: String,
    reply_to: Option<&str>,
    flags: &ReviewCommentFlags,
) -> Result<BoardAction, String> {
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
                .to_owned(),
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
            let record = pick_record(&found.reviews, flags.record.as_deref())?.ok_or(
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
) -> Result<Option<ReviewLocation>, String> {
    let (path, lines) = match (&flags.path, flags.lines) {
        (None, None) if flags.old => return Err("--old needs --path and --line".to_owned()),
        (None, None) => return Ok(None),
        (Some(path), Some(lines)) => (path, lines),
        (Some(_), None) => return Err("--path needs --line <a[-b]>".to_owned()),
        (None, Some(_)) => return Err("--line needs --path <file>".to_owned()),
    };
    let commit = record
        .fields
        .commit
        .clone()
        .ok_or("that review record names no commit to put lines on")?;
    Ok(Some(ReviewLocation {
        path: path.clone(),
        commit,
        start: lines.start,
        end: lines.end,
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
fn resolve_comment<'a>(card: &'a CardView, sel: &str) -> Result<CommentTarget<'a>, String> {
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
        (Some(_), Some(_)) => Err(format!("ambiguous comment prefix '{sel}'")),
        _ => Err(format!("no comment matching '{sel}' on this card")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::ReviewFields;

    /// A review record carrying only `commit`.
    fn review(commit: &str) -> ReviewView {
        ReviewView {
            id: NoteId::new([0; 32]),
            author: [0; 32],
            created_at: 0,
            fields: ReviewFields {
                commit: Some(commit.to_string()),
                ..Default::default()
            },
            comments: Vec::new(),
            sources: Vec::new(),
        }
    }

    /// No prefix takes the newest; a prefix picks the first match; a prefix
    /// matching nothing is an error rather than a silent fallback.
    #[test]
    fn pick_record_by_prefix() {
        let reviews = [review("bbb222"), review("aaa111")];
        assert_eq!(pick_record(&reviews, None).unwrap(), Some(&reviews[0]));
        assert_eq!(
            pick_record(&reviews, Some("aaa")).unwrap(),
            Some(&reviews[1])
        );
        assert!(pick_record(&reviews, Some("ccc")).is_err());
        assert_eq!(pick_record(&[], None).unwrap(), None);
    }

    /// One line or an inclusive 1-based range; `0`, a backwards range and
    /// anything not a number are refused.
    #[test]
    fn parse_lines_takes_a_line_or_a_range() {
        assert_eq!(parse_lines("42").unwrap(), LineRange { start: 42, end: 42 });
        assert_eq!(parse_lines("3-5").unwrap(), LineRange { start: 3, end: 5 });
        for bad in ["0", "5-3", "a", "3-", "-3", ""] {
            assert!(parse_lines(bad).is_err(), "{bad:?}");
        }
    }

    /// `none` and an empty value clear a due date or an estimate; anything
    /// else must parse.
    #[test]
    fn due_and_estimate_clear_with_none() {
        assert_eq!(parse_due("none").unwrap(), None);
        assert_eq!(parse_due(" ").unwrap(), None);
        assert!(parse_due("2026-11-02").unwrap().is_some());
        assert!(parse_due("tomorrow").is_err());
        assert_eq!(parse_estimate("NONE").unwrap(), None);
        assert_eq!(parse_estimate("3").unwrap(), Some(3));
        assert!(parse_estimate("three").is_err());
    }
}
