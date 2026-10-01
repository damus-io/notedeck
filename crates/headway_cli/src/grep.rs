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

use headway::event::{self, BoardView, CardView};

use crate::output::plain_ref;

/// `headway grep <pattern>` — print every line of card text in `boards` that
/// `pattern` matches, grouped under each card's `headway:<board>/<word-id>` ref.
///
/// Each board was folded once by the caller, and this is one pass over each
/// board's cards, so a search costs what a single `show` does however many
/// cards match. The shell equivalent — `show --json`, then `show <card>` per
/// card to reach its comments — re-syncs the relay once per card.
///
/// `subtree` (`--in <card>`) keeps only that card and its descendants, as one
/// set computed before the pass (see [`subtree_ids`]). Archived cards are
/// searched only when `include_archived` (`--archived`) asks for them, like
/// `show` lists them.
///
/// The text output goes through a pager when stdout is a terminal, like
/// `git grep` and `agentium grep` (`--pager`/`--no-pager` force it either
/// way), and is colored for the effective sink unless `--color` says otherwise
/// — `--color always` keeps the highlight when piping into your own `less -R`.
pub(crate) fn cmd_grep(
    boards: &[BoardView],
    subtree: Option<[u8; 32]>,
    pattern: &Regex,
    include_archived: bool,
    out: GrepOutput,
) -> Result<()> {
    let stdout_tty = std::io::stdout().is_terminal();
    let use_pager = !out.json && out.pager.enabled(stdout_tty);
    let color = out.color.enabled(stdout_tty || use_pager);
    let as_json = out.json;
    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut output = String::new();

    for view in boards {
        let within = subtree.map(|root| subtree_ids(view, root));
        for (card, column) in searched_cards(view, include_archived) {
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
            if as_json {
                rows.push(card_json(view, card, column, &matches));
                continue;
            }
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&grep_header(view, card, column, color));
            // Sized to the fields this card actually matched, like agentium
            // sizes its role column, so a card with only `desc` hits isn't
            // padded out to `comment`'s width.
            let width = matches
                .iter()
                .map(|m| m.field.as_str().len())
                .max()
                .unwrap_or(0);
            for m in &matches {
                output.push_str(&grep_match_line(m, pattern, width, color));
            }
        }
    }

    if as_json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
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

/// The cards `grep` reads on `view`, each with where it sits: its column's
/// name, or `archived` (the word `show <card>` uses) for an archived card,
/// which is only included when `include_archived`.
fn searched_cards(
    view: &BoardView,
    include_archived: bool,
) -> impl Iterator<Item = (&CardView, &str)> {
    let live = view
        .columns
        .iter()
        .flat_map(|col| col.cards.iter().map(move |c| (c, col.name.as_str())));
    let archived = view
        .archived
        .iter()
        .filter(move |_| include_archived)
        .map(|a| (&a.card, "archived"));
    live.chain(archived)
}

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

/// One matching line of a card's text, and the field it came from.
#[derive(Debug, PartialEq, Eq)]
struct GrepMatch {
    field: Field,
    text: String,
}

/// Every line of `card`'s text that `pattern` matches, in reading order: the
/// title, the description, the comment thread, then the review comments.
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
    let comments = card.comments.iter().map(|c| (Field::Comment, &c.body));
    let reviews = card
        .reviews
        .iter()
        .flat_map(|r| r.comments.iter().map(|c| (Field::Review, &c.body)));
    [
        (Field::Title, &card.title),
        (Field::Description, &card.description),
    ]
    .into_iter()
    .chain(comments)
    .chain(reviews)
    .flat_map(|(field, text)| {
        text.lines()
            .filter(|line| pattern.is_match(line))
            .map(move |line| GrepMatch {
                field,
                text: line.trim_end().to_string(),
            })
    })
    .collect()
}

/// The `grep --json` shape for one card: its identity — the same `ref`/`id`
/// `next --json` emits, plus its board, title and column (`archived` for an
/// archived card, as `show <card> --json` says) — and its matching lines.
/// Grouped rather than one row per match, so a card's identity isn't repeated
/// once per hit.
fn card_json(
    view: &BoardView,
    card: &CardView,
    column: &str,
    matches: &[GrepMatch],
) -> serde_json::Value {
    let matches: Vec<_> = matches
        .iter()
        .map(|m| json!({ "field": m.field.as_str(), "text": m.text }))
        .collect();
    json!({
        "ref": plain_ref(view, &card.id),
        "id": card.id.hex(),
        "board": view.id,
        "title": card.title,
        "column": column,
        "matches": matches,
    })
}

/// The header line introducing a card's matches: its full ref (bold, and
/// untruncated so it pastes straight into `show`/`comment`), its title, and
/// the column it sits in, dimmed.
fn grep_header(view: &BoardView, card: &CardView, column: &str, color: bool) -> String {
    format!(
        "{}  {}  {}\n",
        paint(color, SGR_BOLD, &plain_ref(view, &card.id)),
        card.title,
        paint(color, SGR_DIM, &format!("({column})")),
    )
}

/// One match row: the indented field label padded to `width`, then the
/// matching line with every occurrence of the pattern highlighted.
fn grep_match_line(m: &GrepMatch, pattern: &Regex, width: usize, color: bool) -> String {
    let label = format!("{:<width$}", m.field.as_str());
    format!(
        "  {}  {}\n",
        paint(color, SGR_FIELD, &label),
        highlight(pattern, &m.text, color),
    )
}

/// Bold, for the card ref leading each group.
const SGR_BOLD: &str = "1";
/// Dim grey, as `show` dims its refs, for the column after a card's title.
const SGR_DIM: &str = "90";
/// The field label is dimmed too, so the matched text is what stands out.
const SGR_FIELD: &str = SGR_DIM;

#[cfg(test)]
mod tests {
    use super::*;

    use cli_term::{CaseMode, compile_pattern};
    use headway::event::{ArchivedCard, ColumnView, CommentView, Priority, SubissueView};
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
        CommentView {
            id: id(n),
            author: [0; 32],
            parent: None,
            body: body.to_string(),
            created_at: 0,
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
    fn archived_cards_are_searched_only_when_asked() {
        let view = board(vec![card(1, "live", "")], vec![card(2, "gone", "")]);
        let titles = |include| -> Vec<(String, String)> {
            searched_cards(&view, include)
                .map(|(c, col)| (c.title.clone(), col.to_string()))
                .collect()
        };
        assert_eq!(titles(false), vec![("live".into(), "Todo".into())]);
        assert_eq!(
            titles(true),
            vec![
                ("live".into(), "Todo".into()),
                ("gone".into(), "archived".into())
            ]
        );
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

        let m = GrepMatch {
            field: Field::Comment,
            text: "the relay is back".to_string(),
        };
        let line = grep_match_line(&m, &re("relay"), 7, false);
        assert_eq!(line, "  comment  the relay is back\n");
        let m = GrepMatch {
            field: Field::Title,
            text: "relay".to_string(),
        };
        assert_eq!(
            grep_match_line(&m, &re("relay"), 7, false),
            "  title    relay\n"
        );
    }
}
