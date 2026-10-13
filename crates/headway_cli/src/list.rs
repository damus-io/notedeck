//! `headway list` — the cards a query keeps, one row each, across every board.
//!
//! The query is [`headway::query`]'s, the same one the GUI board filter reads,
//! so `p:high label:ui is:done` means one thing everywhere. Output follows
//! `grep`: paged and colored on a terminal, plain when piped, `--json` for
//! scripts.

use std::io::IsTerminal;

use cli_term::{ColorWhen, PagerMode, paint};
use nostrdb_net::relay::sync::Result;
use serde_json::json;

use headway::event::{BoardView, Priority, card_json};
use headway::query::{IssueFilter, ListQuery, ListRow, StateFilter, list};

use crate::grep::{PAGER_VAR, SGR_DIM, SGR_REF, SGR_TITLE};
use crate::output::{BLOCKED_GLYPH, labels_suffix, plain_ref, priority_glyph};

/// How `list`'s rows are written: `--json`, or text under the `--color` and
/// `--pager`/`--no-pager` modes, and at most `limit` of them (`-n`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ListOutput {
    pub(crate) json: bool,
    pub(crate) color: ColorWhen,
    pub(crate) pager: PagerMode,
    pub(crate) limit: Option<usize>,
}

/// `headway list [query...]` — print every live card on `boards` that `query`
/// keeps, in board order (board, then column, then rank), one row per card.
///
/// `query` is parsed by [`IssueFilter::parse`] unscoped, so a pasted
/// `headway:<board>/<word-id>` names exactly that card. With no `is:` term the
/// list is the open cards (Linear's "Active"); `include_done` (`--all`) drops
/// that default. `subtree` (`--in <card>`) keeps that card and its descendants.
pub(crate) fn cmd_list(
    boards: &[BoardView],
    subtree: Option<[u8; 32]>,
    query: &str,
    include_done: bool,
    out: ListOutput,
) -> Result<()> {
    let query = ListQuery {
        filter: default_state(IssueFilter::parse(query, None), include_done),
        within: subtree,
    };
    let mut rows = list(boards, &query);
    if let Some(n) = out.limit {
        rows.truncate(n);
    }

    if out.json {
        let rows: Vec<serde_json::Value> = rows.iter().map(row_json).collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("no cards");
        return Ok(());
    }
    let stdout_tty = std::io::stdout().is_terminal();
    let use_pager = out.pager.enabled(stdout_tty);
    let color = out.color.enabled(stdout_tty || use_pager);
    let widths = RowWidths::of(&rows);
    let output: String = rows
        .iter()
        .map(|row| list_line(row, widths, color))
        .collect();
    cli_term::emit(&output, use_pager, PAGER_VAR);
    Ok(())
}

/// `filter` with the list's default state: open cards, unless the query says
/// which state it wants or `include_done` asks for every state.
fn default_state(mut filter: IssueFilter, include_done: bool) -> IssueFilter {
    if filter.state.is_none() && !include_done {
        filter.state = Some(StateFilter::Open);
    }
    filter
}

/// The `list --json` shape for one row: the card as `show --json` renders it,
/// plus the board and column it sits in.
fn row_json(row: &ListRow) -> serde_json::Value {
    let mut card = card_json(&row.board.id, row.card);
    card["board"] = json!(row.board.id);
    card["column"] = json!(row.column.name);
    card
}

/// The padded widths of a listing's cells, so every ref and every title
/// starts in one column. A marker slot is only there when some row has that
/// marker, so a listing with none starts flush with the ref.
#[derive(Clone, Copy, Debug)]
struct RowWidths {
    /// Some row is blocked: every row gets a [`BLOCKED_GLYPH`]-wide slot.
    blocked: bool,
    /// Some row has a priority: every row gets a priority-glyph-wide slot.
    priority: bool,
    reference: usize,
    column: usize,
}

impl RowWidths {
    /// The widths `rows` need.
    fn of(rows: &[ListRow]) -> RowWidths {
        let mut widths = RowWidths {
            blocked: false,
            priority: false,
            reference: 0,
            column: 0,
        };
        for row in rows {
            widths.blocked |= row.card.is_blocked();
            widths.priority |= row.card.priority != Priority::None;
            let reference = plain_ref(row.board, &row.card.id).chars().count();
            widths.reference = widths.reference.max(reference);
            widths.column = widths.column.max(row.column.name.chars().count() + 2);
        }
        widths
    }
}

/// One row: the blocked and priority markers, the card's full ref (yellow,
/// pasteable), its `[column]` dimmed, then its title in bold with its labels
/// and subissue rollup after it.
fn list_line(row: &ListRow, widths: RowWidths, color: bool) -> String {
    let card = row.card;
    let blocked = match (widths.blocked, card.is_blocked()) {
        (_, true) => paint(color, SGR_DIM, BLOCKED_GLYPH),
        (true, false) => MARKER_GAP.to_string(),
        (false, false) => String::new(),
    };
    let priority = match (widths.priority, card.priority) {
        (true, Priority::None) => MARKER_GAP.to_string(),
        (false, Priority::None) => String::new(),
        // Urgent is the one marker drawn at full strength, as `show` does.
        (_, Priority::Urgent) => priority_glyph(card.priority).to_string(),
        (_, p) => paint(color, SGR_DIM, priority_glyph(p)),
    };
    let reference = format!(
        "{:<width$}",
        plain_ref(row.board, &card.id),
        width = widths.reference
    );
    let column = format!(
        "{:<width$}",
        format!("[{}]", row.column.name),
        width = widths.column
    );
    let progress = if card.subissues.is_empty() {
        String::new()
    } else {
        let done = card.subissues.iter().filter(|s| s.done).count();
        format!(
            "  {}",
            paint(color, SGR_DIM, &format!("{done}/{}", card.subissues.len()))
        )
    };
    format!(
        "{}{}{}  {}  {}{}{}\n",
        blocked,
        priority,
        paint(color, SGR_REF, &reference),
        paint(color, SGR_DIM, &column),
        paint(color, SGR_TITLE, &card.title),
        labels_suffix(&card.labels),
        progress,
    )
}

/// The blank a row without a marker puts in that marker's slot: as wide as
/// every glyph (one character and a space).
const MARKER_GAP: &str = "  ";

#[cfg(test)]
mod tests {
    use super::*;

    use headway::event::{CardView, ColumnView};
    use nostrdb_net::NoteId;

    fn card(n: u8, title: &str) -> CardView {
        CardView {
            id: NoteId::new([n; 32]),
            author: [0; 32],
            title: title.to_string(),
            description: String::new(),
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

    fn board(cards: Vec<CardView>) -> BoardView {
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
            archived: Vec::new(),
            deleted: false,
        }
    }

    /// No `is:` term means open cards; `--all` or any `is:` term overrides it.
    #[test]
    fn open_is_the_default_state() {
        let parsed = |q: &str, all| default_state(IssueFilter::parse(q, None), all).state;
        assert_eq!(parsed("relay", false), Some(StateFilter::Open));
        assert_eq!(parsed("relay", true), None);
        assert_eq!(parsed("is:done", false), Some(StateFilter::Done));
        assert_eq!(parsed("is:any", false), Some(StateFilter::Any));
    }

    /// A plain row: markers, padded ref, padded column, title, labels.
    #[test]
    fn plain_row_lines_up_and_leads_with_the_ref() {
        let mut high = card(1, "relay reconnect");
        high.priority = Priority::High;
        high.labels = vec!["ui".to_string()];
        let view = board(vec![high, card(2, "other")]);
        let column = &view.columns[0];
        let rows: Vec<ListRow> = column
            .cards
            .iter()
            .map(|card| ListRow {
                board: &view,
                card,
                column,
            })
            .collect();
        let widths = RowWidths::of(&rows);
        let line = list_line(&rows[0], widths, false);
        assert!(!line.contains('\x1b'), "{line:?}");
        assert_eq!(
            line,
            format!(
                "↑ {:<width$}  [Todo]  relay reconnect  [ui]\n",
                plain_ref(&view, &rows[0].card.id),
                width = widths.reference,
            )
        );
        // An unprioritised row keeps the slot blank, so the refs line up.
        assert!(
            list_line(&rows[1], widths, false).starts_with("  headway:work/"),
            "{:?}",
            list_line(&rows[1], widths, false)
        );
        // With no priority anywhere there is no slot at all.
        let flush = RowWidths::of(&rows[1..]);
        assert!(list_line(&rows[1], flush, false).starts_with("headway:work/"));
        let json = row_json(&rows[0]);
        assert_eq!(json["column"], "Todo");
        assert_eq!(json["board"], "work");
        assert_eq!(json["priority"], "high");
    }
}
