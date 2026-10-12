//! `headway grep` — search card text across a board (or every board).
//!
//! Modelled on `agentium grep` (crates/agentium_cli/src/grep.rs): each matching
//! line printed under its owner's full, pasteable ref, smart-case by default,
//! paged and colored the same way, and `--json` grouped per owner rather than
//! one flat row per hit. The smart-case compilation, highlighting, pager and
//! `--color`/`--pager` modes are agentium's own, shared through [`cli_term`].

use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;

use cli_term::{ColorWhen, PagerMode, highlight, paint};
use nostrdb_net::relay::sync::Result;
use regex::Regex;
use serde_json::json;

use headway::event::{self, ActivityKind, BoardView, CardView};
use headway::fmt::rel_time;

use crate::output::{PlacedCard, placed_cards, plain_ref};

/// `headway grep <pattern>` — print every line of card text in `boards` that
/// `pattern` matches, grouped under each card's `headway:<board>/<word-id>` ref,
/// each line with how long ago its text was written. Cards come most recent
/// first: ordered by their newest matching line, across every board searched,
/// so the live discussion leads and a stale mention sinks.
///
/// Each board was folded once by the caller, and this is one pass over each
/// board's cards, so a search costs what a single `show` does however many
/// cards match. The shell equivalent — `show --json`, then `show <card>` per
/// card to reach its comments — re-syncs the relay once per card.
///
/// `subtree` (`--in <card>`) keeps only that card and its descendants, as one
/// set computed before the pass (see [`subtree_ids`]). Archived cards are
/// always searched — a search that silently skips them misses exactly the old
/// discussion it's usually after — and are marked `archived` where a live card
/// shows its column.
///
/// The text output goes through a pager when stdout is a terminal, like
/// `git grep` and `agentium grep` (`--pager`/`--no-pager` force it either
/// way), and is colored for the effective sink unless `--color` says otherwise
/// — `--color always` keeps the highlight when piping into your own `less -R`.
pub(crate) fn cmd_grep(
    boards: &[BoardView],
    subtree: Option<[u8; 32]>,
    pattern: &Regex,
    out: GrepOutput,
) -> Result<()> {
    let stdout_tty = std::io::stdout().is_terminal();
    let use_pager = !out.json && out.pager.enabled(stdout_tty);
    let color = out.color.enabled(stdout_tty || use_pager);

    let mut hits: Vec<CardHit> = Vec::new();
    for view in boards {
        let within = subtree.map(|root| subtree_ids(view, root));
        for PlacedCard { card, column } in placed_cards(view) {
            if within
                .as_ref()
                .is_some_and(|ids| !ids.contains(card.id.bytes()))
            {
                continue;
            }
            let matches = card_matches(card, pattern);
            if matches.is_empty() {
                continue;
            }
            hits.push(CardHit {
                view,
                card,
                column,
                matches,
            });
        }
    }
    sort_most_recent_first(&mut hits);

    if out.json {
        let rows: Vec<serde_json::Value> = hits.iter().map(card_json).collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    let mut output = String::new();
    for hit in &hits {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&grep_header(hit.view, hit.card, hit.column, color));
        // Sized to what this card actually matched, like agentium sizes its
        // role column, so a card with only `desc` hits isn't padded out to
        // `comment`'s width.
        let width = MatchWidths::of(&hit.matches);
        for m in &hit.matches {
            output.push_str(&grep_match_line(m, pattern, width, color));
        }
    }
    if output.is_empty() {
        println!("no matches");
        return Ok(());
    }
    cli_term::emit(&output, use_pager, PAGER_VAR);
    Ok(())
}

/// How `grep`'s results are written: `--json`, or text under the `--color`
/// and `--pager`/`--no-pager` modes. JSON is never paged or colored.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GrepOutput {
    pub(crate) json: bool,
    pub(crate) color: ColorWhen,
    pub(crate) pager: PagerMode,
}

/// The variable naming headway's own pager command, consulted before `$PAGER`
/// by [`cli_term::emit`] — the counterpart of agentium's `$AGENTIUM_PAGER`.
const PAGER_VAR: &str = "HEADWAY_PAGER";

/// `root` and every card below it on `view`, live or archived — the set
/// `--in <card>` narrows the search to.
///
/// Unlike `next --in`, the container itself is in the set: `next` hands out
/// the container's *members*, but "search this epic" means the epic's own
/// text too. Archived descendants are in the set as well; whether they are
/// searched is still `--archived`'s call. A subissue cycle that slipped past
/// the write-time guard terminates here, since a card is visited once.
fn subtree_ids(view: &BoardView, root: [u8; 32]) -> HashSet<[u8; 32]> {
    let by_id: HashMap<[u8; 32], &CardView> =
        event::all_cards(view).map(|c| (*c.id.bytes(), c)).collect();
    let mut seen = HashSet::from([root]);
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let Some(card) = by_id.get(&id) else {
            continue;
        };
        for sub in &card.subissues {
            if seen.insert(*sub.id.bytes()) {
                stack.push(*sub.id.bytes());
            }
        }
    }
    seen
}

/// Which part of a card a matching line came from — `grep`'s analogue of
/// agentium's message role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Title,
    Description,
    /// A comment in the card's thread.
    Comment,
    /// An inline review comment on one of the card's review records' commits.
    Review,
}

impl Field {
    /// The label printed beside the line, and the `field` value in `--json`.
    fn as_str(self) -> &'static str {
        match self {
            Field::Title => "title",
            Field::Description => "desc",
            Field::Comment => "comment",
            Field::Review => "review",
        }
    }
}

/// One matching line of a card's text, the field it came from, and when that
/// text was written (see [`card_matches`]).
#[derive(Debug, PartialEq, Eq)]
struct GrepMatch {
    field: Field,
    text: String,
    /// Unix seconds: the comment's own time, or for the title and description
    /// the edit that last set them.
    created_at: u64,
}

/// A card with at least one match, gathered across every board searched so
/// the results can be ordered as a whole before any is rendered.
struct CardHit<'v> {
    view: &'v BoardView,
    card: &'v CardView,
    /// The column it sits in, or `archived`.
    column: &'v str,
    /// Its matching lines, in reading order. Never empty.
    matches: Vec<GrepMatch>,
}

impl CardHit<'_> {
    /// When the newest of this card's matching lines was written — what the
    /// results are ordered by.
    fn newest(&self) -> u64 {
        self.matches.iter().map(|m| m.created_at).max().unwrap_or(0)
    }
}

/// Order `hits` most recent first, by each card's newest matching line. The
/// sort is stable, so cards whose newest lines tie (same-second edits) keep
/// board-then-column order.
///
/// The newest *match*, not the card's `updated_at`: a card whose description
/// mentioned the pattern a month ago and was only moved yesterday is a month-old
/// hit, and putting it first would bury the cards still talking about it.
fn sort_most_recent_first(hits: &mut [CardHit]) {
    hits.sort_by_key(|hit| std::cmp::Reverse(hit.newest()));
}

/// Every line of `card`'s text that `pattern` matches, in reading order: the
/// title, the description, the comment thread, then the review comments.
///
/// Each line carries when its text was written. A comment has its own time;
/// the title and description are dated by the edit that last set them — the
/// newest rename or description edit in the card's activity — falling back to
/// the card's creation when it was never edited.
///
/// What a card's text is: what people wrote on it. Labels are left out — they
/// are short tags rather than prose, and a hit on `bug` in every bug-labelled
/// card would bury the cards that *say* bug. So are the review records'
/// commit subjects and paths, which are git's metadata about the card, not
/// text written on it.
///
/// Matched per line, like `grep`: a long description contributes the lines
/// that match, not its whole body.
fn card_matches(card: &CardView, pattern: &Regex) -> Vec<GrepMatch> {
    let last_set = |is_edit: fn(&ActivityKind) -> bool| {
        card.activity
            .iter()
            .rev()
            .find(|a| is_edit(&a.kind))
            .map_or(card.created_at, |a| a.created_at)
    };
    let title_at = last_set(|k| matches!(k, ActivityKind::Renamed { .. }));
    let desc_at = last_set(|k| matches!(k, ActivityKind::DescriptionEdited));
    let comments = card
        .comments
        .iter()
        .map(|c| (Field::Comment, &c.body, c.created_at));
    let reviews = card.reviews.iter().flat_map(|r| {
        r.comments
            .iter()
            .map(|c| (Field::Review, &c.body, c.created_at))
    });
    [
        (Field::Title, &card.title, title_at),
        (Field::Description, &card.description, desc_at),
    ]
    .into_iter()
    .chain(comments)
    .chain(reviews)
    .flat_map(|(field, text, created_at)| {
        text.lines()
            .filter(|line| pattern.is_match(line))
            .map(move |line| GrepMatch {
                field,
                text: line.trim_end().to_string(),
                created_at,
            })
    })
    .collect()
}

/// The `grep --json` shape for one card: its identity — the same `ref`/`id`
/// `next --json` emits, plus its board, title and column (`archived` for an
/// archived card, as `show <card> --json` says) — and its matching lines, each
/// with the unix `created_at` the text output renders as "2h ago". Grouped
/// rather than one row per match, so a card's identity isn't repeated once per
/// hit.
fn card_json(hit: &CardHit) -> serde_json::Value {
    let matches: Vec<_> = hit
        .matches
        .iter()
        .map(|m| {
            json!({
                "field": m.field.as_str(),
                "text": m.text,
                "created_at": m.created_at,
            })
        })
        .collect();
    json!({
        "ref": plain_ref(hit.view, &hit.card.id),
        "id": hit.card.id.hex(),
        "board": hit.view.id,
        "title": hit.card.title,
        "column": hit.column,
        "matches": matches,
    })
}

/// The header line introducing a card's matches, styled like a `git log
/// --oneline` row: its full ref in yellow where the commit hash would be
/// (untruncated, so it pastes straight into `show`/`comment`), its title in
/// bold, and the column it sits in, dimmed. The ref and title used to share
/// one weight, which read as a single run with no visible title.
fn grep_header(view: &BoardView, card: &CardView, column: &str, color: bool) -> String {
    format!(
        "{}  {}  {}\n",
        paint(color, SGR_REF, &plain_ref(view, &card.id)),
        paint(color, SGR_TITLE, &card.title),
        paint(color, SGR_DIM, &format!("({column})")),
    )
}

/// The padded widths of a card's match rows: its longest field label and its
/// longest relative time, so the matched text starts in one column per card.
#[derive(Clone, Copy, Debug)]
struct MatchWidths {
    field: usize,
    time: usize,
}

impl MatchWidths {
    /// The widths `matches` need.
    fn of(matches: &[GrepMatch]) -> MatchWidths {
        let mut widths = MatchWidths { field: 0, time: 0 };
        for m in matches {
            widths.field = widths.field.max(m.field.as_str().len());
            widths.time = widths.time.max(rel_time(m.created_at).len());
        }
        widths
    }
}

/// One match row: the indented field label and the line's relative time, both
/// padded and dimmed, then the matching line with every occurrence of the
/// pattern highlighted.
fn grep_match_line(m: &GrepMatch, pattern: &Regex, widths: MatchWidths, color: bool) -> String {
    let label = format!("{:<width$}", m.field.as_str(), width = widths.field);
    let when = format!("{:<width$}", rel_time(m.created_at), width = widths.time);
    format!(
        "  {}  {}  {}\n",
        paint(color, SGR_FIELD, &label),
        paint(color, SGR_DIM, &when),
        highlight(pattern, &m.text, color),
    )
}

/// Yellow, `git log`'s commit-hash color, for the card ref leading each group.
const SGR_REF: &str = "33";
/// Bold, so the card's title is what the eye lands on in each header.
const SGR_TITLE: &str = "1";
/// Dim grey, as `show` dims its refs, for the column after a card's title.
const SGR_DIM: &str = "90";
/// The field label is dimmed too, so the matched text is what stands out.
const SGR_FIELD: &str = SGR_DIM;

#[cfg(test)]
mod tests {
    use super::*;

    use cli_term::{CaseMode, compile_pattern};
    use headway::event::{
        ActivityView, ArchivedCard, ColumnView, CommentView, Priority, SubissueView,
    };
    use nostrdb_net::NoteId;

    fn id(n: u8) -> NoteId {
        NoteId::new([n; 32])
    }

    fn card(n: u8, title: &str, description: &str) -> CardView {
        CardView {
            id: id(n),
            author: [0; 32],
            title: title.to_string(),
            description: description.to_string(),
            labels: Vec::new(),
            priority: Priority::None,
            due: None,
            estimate: None,
            rank: String::new(),
            seq: None,
            placed_at: 0,
            created_at: 0,
            updated_at: 0,
            comments: Vec::new(),
            reviews: Vec::new(),
            activity: Vec::new(),
            parent: None,
            subissues: Vec::new(),
            blocked_by: Vec::new(),
            blocks: Vec::new(),
            related: Vec::new(),
        }
    }

    fn comment(n: u8, body: &str) -> CommentView {
        comment_at(n, body, 0)
    }

    fn comment_at(n: u8, body: &str, created_at: u64) -> CommentView {
        CommentView {
            id: id(n),
            author: [0; 32],
            parent: None,
            body: body.to_string(),
            created_at,
        }
    }

    fn activity(kind: ActivityKind, created_at: u64) -> ActivityView {
        ActivityView {
            author: [0; 32],
            created_at,
            kind,
        }
    }

    fn sub(n: u8) -> SubissueView {
        SubissueView {
            id: id(n),
            title: String::new(),
            column: None,
            done: false,
            archived: false,
            seq: None,
        }
    }

    fn board(cards: Vec<CardView>, archived: Vec<CardView>) -> BoardView {
        BoardView {
            id: "work".to_string(),
            author: [0; 32],
            title: "Work".to_string(),
            description: String::new(),
            created_at: 0,
            columns: vec![ColumnView {
                id: "todo".to_string(),
                name: "Todo".to_string(),
                terminal: false,
                cards,
            }],
            archived: archived
                .into_iter()
                .map(|card| ArchivedCard { card, from: None })
                .collect(),
            deleted: false,
        }
    }

    fn re(pattern: &str) -> Regex {
        compile_pattern(pattern, CaseMode::Smart).unwrap()
    }

    /// Title, description, comments and review comments are searched, per
    /// line and in that order; labels are not.
    #[test]
    fn card_matches_reads_each_field_per_line() {
        let mut c = card(1, "relay reconnect", "first line\nthe relay stalls\nlast");
        c.labels = vec!["relay".to_string()];
        c.comments = vec![comment(2, "no hit"), comment(3, "relay is back")];
        let matches = card_matches(&c, &re("relay"));
        let got: Vec<(Field, &str)> = matches.iter().map(|m| (m.field, m.text.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (Field::Title, "relay reconnect"),
                (Field::Description, "the relay stalls"),
                (Field::Comment, "relay is back"),
            ]
        );
        assert!(card_matches(&card(4, "x", ""), &re("relay")).is_empty());
    }

    /// A comment is dated by itself; the title and description by the edit
    /// that last set them, else by the card's creation.
    #[test]
    fn card_matches_date_each_line_by_when_it_was_written() {
        let mut c = card(1, "relay reconnect", "the relay stalls");
        c.created_at = 100;
        c.comments = vec![comment_at(2, "relay is back", 900)];
        let times = |c: &CardView| -> Vec<(Field, u64)> {
            card_matches(c, &re("relay"))
                .iter()
                .map(|m| (m.field, m.created_at))
                .collect()
        };
        assert_eq!(
            times(&c),
            vec![
                (Field::Title, 100),
                (Field::Description, 100),
                (Field::Comment, 900),
            ]
        );

        // The newest edit of each kind wins; other activity doesn't count.
        c.activity = vec![
            activity(ActivityKind::DescriptionEdited, 200),
            activity(
                ActivityKind::Renamed {
                    to: "relay reconnect".to_string(),
                },
                300,
            ),
            activity(ActivityKind::DescriptionEdited, 400),
            activity(ActivityKind::Archived, 500),
        ];
        assert_eq!(
            times(&c),
            vec![
                (Field::Title, 300),
                (Field::Description, 400),
                (Field::Comment, 900),
            ]
        );
    }

    /// Cards come newest first, by their newest matching line — not by when
    /// the card itself last changed — and ties keep the board's order.
    #[test]
    fn hits_sort_by_their_newest_match() {
        let mut old_card_new_comment = card(1, "relay one", "");
        old_card_new_comment.comments = vec![comment_at(9, "relay again", 500)];
        let mut stale_mention = card(2, "relay two", "");
        stale_mention.created_at = 100;
        stale_mention.updated_at = 1_000;
        let mut tie_a = card(3, "relay three", "");
        tie_a.created_at = 300;
        let mut tie_b = card(4, "relay four", "");
        tie_b.created_at = 300;
        let view = board(
            vec![stale_mention, tie_a, old_card_new_comment, tie_b],
            Vec::new(),
        );
        let pattern = re("relay");
        let mut hits: Vec<CardHit> = placed_cards(&view)
            .map(|PlacedCard { card, column }| CardHit {
                view: &view,
                card,
                column,
                matches: card_matches(card, &pattern),
            })
            .collect();
        sort_most_recent_first(&mut hits);
        let order: Vec<&str> = hits.iter().map(|h| h.card.title.as_str()).collect();
        assert_eq!(
            order,
            vec!["relay one", "relay three", "relay four", "relay two"]
        );
    }

    /// `--in` keeps the container and everything under it, archived
    /// descendants included, and nothing beside it.
    #[test]
    fn subtree_holds_the_container_and_its_descendants() {
        let mut epic = card(1, "epic", "");
        epic.subissues = vec![sub(2)];
        let mut child = card(2, "child", "");
        child.subissues = vec![sub(3)];
        let grandchild = card(3, "grandchild", "");
        let other = card(4, "other", "");
        let view = board(vec![epic, child, other], vec![grandchild]);

        let ids = subtree_ids(&view, [1; 32]);
        assert_eq!(ids, HashSet::from([[1; 32], [2; 32], [3; 32]]));
        assert_eq!(subtree_ids(&view, [4; 32]), HashSet::from([[4; 32]]));
    }

    #[test]
    fn archived_cards_are_searched_and_marked() {
        let view = board(vec![card(1, "live", "")], vec![card(2, "gone", "")]);
        let placed: Vec<String> = placed_cards(&view)
            .map(|p| format!("{} in {}", p.card.title, p.column))
            .collect();
        assert_eq!(placed, ["live in Todo", "gone in archived"]);
    }

    #[test]
    fn plain_output_leads_with_the_full_ref() {
        let view = board(vec![card(1, "relay reconnect", "")], Vec::new());
        let c = &view.columns[0].cards[0];
        let header = grep_header(&view, c, "Todo", false);
        assert!(!header.contains('\x1b'), "{header:?}");
        assert!(header.starts_with(&format!("{}  relay reconnect", plain_ref(&view, &c.id))));
        assert!(header.starts_with("headway:work/"), "{header:?}");
        assert!(header.ends_with("(Todo)\n"), "{header:?}");

        // Colored, the ref, title and column each get their own style, so the
        // title reads as a title rather than a continuation of the ref.
        let painted = grep_header(&view, c, "Todo", true);
        let expected = format!(
            "{}  {}  {}\n",
            paint(true, SGR_REF, &plain_ref(&view, &c.id)),
            paint(true, SGR_TITLE, "relay reconnect"),
            paint(true, SGR_DIM, "(Todo)"),
        );
        assert_eq!(painted, expected);
        assert_ne!(SGR_REF, SGR_TITLE);

        // Each row: the field and its relative time, both padded to the
        // card's widest, then the line.
        headway::fmt::freeze_now(10_000);
        let rows = [
            GrepMatch {
                field: Field::Comment,
                text: "the relay is back".to_string(),
                created_at: 10_000 - 2 * 3600,
            },
            GrepMatch {
                field: Field::Title,
                text: "relay".to_string(),
                created_at: 10_000,
            },
        ];
        let widths = MatchWidths::of(&rows);
        let [comment, title] = &rows;
        assert_eq!(
            grep_match_line(comment, &re("relay"), widths, false),
            "  comment  2h ago    the relay is back\n"
        );
        assert_eq!(
            grep_match_line(title, &re("relay"), widths, false),
            "  title    just now  relay\n"
        );
    }
}
