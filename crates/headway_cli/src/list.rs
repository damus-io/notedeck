//! `headway list` — the cards a query keeps, one row each, across every board.
//!
//! The query is [`headway::query`]'s, the same one the GUI board filter reads,
//! so `p:high label:ui is:done` means one thing everywhere. Output follows
//! `grep`: paged and colored on a terminal, plain when piped, `--json` for
//! scripts.

use std::collections::HashSet;
use std::io::IsTerminal;

use cli_term::{ColorWhen, PagerMode, paint};
use nostrdb_net::relay::sync::Result;
use serde_json::json;

use headway::event::{BoardView, Priority, card_json};
use headway::fmt::rel_time;
use headway::query::{
    DisplayProp, DisplayProps, Grouping, IssueFilter, ListGroup, ListQuery, ListRow, SortBy,
    Sorting, StateFilter, group, list, ready_set,
};

use crate::grep::{PAGER_VAR, SGR_DIM, SGR_REF, SGR_TITLE};
use crate::output::{BLOCKED_GLYPH, labels_suffix, plain_ref, priority_glyph};

/// How `list`'s rows are written: `--json`, or text under the `--color` and
/// `--pager`/`--no-pager` modes, at most `limit` of them (`-n`), each text
/// row printing the `display` properties (`--show`/`--hide`), gathered under
/// `grouping`'s headers (`--group`/`--sub-group`/`--empty`), in `sorting`'s
/// order (`--sort`, `--descending`/`--ascending`, `--completed-by-recency`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ListOutput {
    pub(crate) json: bool,
    pub(crate) color: ColorWhen,
    pub(crate) pager: PagerMode,
    pub(crate) limit: Option<usize>,
    pub(crate) display: DisplayProps,
    pub(crate) grouping: Grouping,
    pub(crate) sorting: Sorting,
}

/// `headway list [query...]` — print every live card on `boards` that `query`
/// keeps, one row per card, in `out.sorting`'s order: board order (board,
/// then column, then rank) unless `--sort` says otherwise. `-n` keeps the
/// first rows of that order. Sorted by work order, the rows `headway next
/// --ready` would hand out are marked.
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
        display: out.display,
        grouping: out.grouping,
        sorting: out.sorting,
    };
    let mut rows = list(boards, &query);
    if let Some(n) = out.limit {
        rows.truncate(n);
    }
    let ready = (query.sorting.by == SortBy::WorkOrder).then(|| ready_set(&rows));
    let marks = RowMarks {
        ready: ready.as_ref(),
    };
    let groups = query.grouping.is_grouped().then(|| {
        let boards: Vec<&BoardView> = boards.iter().collect();
        group(&boards, &rows, query.grouping)
    });

    if out.json {
        let json = match &groups {
            Some(groups) => groups_json(&rows, groups, marks),
            None => rows.iter().map(|row| row_json(row, marks)).collect(),
        };
        println!("{}", serde_json::to_string_pretty(&json)?);
        return Ok(());
    }
    if groups.as_ref().map_or(rows.is_empty(), Vec::is_empty) {
        println!("no cards");
        return Ok(());
    }
    let stdout_tty = std::io::stdout().is_terminal();
    let use_pager = out.pager.enabled(stdout_tty);
    let color = out.color.enabled(stdout_tty || use_pager);
    let widths = RowWidths::of(&rows, query.grouping.row_display(query.display), marks);
    let output = match &groups {
        Some(groups) => {
            let mut output = String::new();
            for g in groups {
                group_lines(&mut output, &rows, g, widths, marks, color, "");
            }
            output
        }
        None => rows
            .iter()
            .map(|row| list_line(row, widths, marks, color))
            .collect(),
    };
    cli_term::emit(&output, use_pager, PAGER_VAR);
    Ok(())
}

/// Write group `g` to `output`: a `## <label> (<count>)` header, then its
/// rows, or its sub-groups' headers and rows indented under it. Groups after
/// the first open with a blank line, so each reads as a section.
fn group_lines(
    output: &mut String,
    rows: &[ListRow],
    g: &ListGroup,
    widths: RowWidths,
    marks: RowMarks,
    color: bool,
    indent: &str,
) {
    if !output.is_empty() {
        output.push('\n');
    }
    let header = format!("## {} ({})", g.label, g.rows.len());
    output.push_str(indent);
    output.push_str(&paint(color, SGR_TITLE, &header));
    output.push('\n');
    if g.subgroups.is_empty() {
        for &at in &g.rows {
            output.push_str(indent);
            output.push_str(&list_line(&rows[at], widths, marks, color));
        }
        return;
    }
    let inner = format!("{indent}  ");
    for sub in &g.subgroups {
        group_lines(output, rows, sub, widths, marks, color, &inner);
    }
}

/// The `list --json` shape of a grouped list: one object per group, its
/// header as `group`, its row count, its rows as [`row_json`] renders them
/// (every row of the group, sub-grouped or not) and its sub-groups, the same
/// shape again.
fn groups_json(rows: &[ListRow], groups: &[ListGroup], marks: RowMarks) -> serde_json::Value {
    groups
        .iter()
        .map(|g| {
            json!({
                "group": g.label,
                "count": g.rows.len(),
                "rows": g.rows.iter().map(|&at| row_json(&rows[at], marks)).collect::<Vec<_>>(),
                "subgroups": groups_json(rows, &g.subgroups, marks),
            })
        })
        .collect()
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
/// plus the board and column it sits in, and whether it's ready when the
/// list marks that.
fn row_json(row: &ListRow, marks: RowMarks) -> serde_json::Value {
    let mut card = card_json(&row.board.id, row.card);
    card["board"] = json!(row.board.id);
    card["column"] = json!(row.place_name());
    if let Some(ready) = marks.ready {
        card["ready"] = json!(ready.contains(row.card.id.bytes()));
    }
    card
}

/// What a listing marks on its rows beyond the card itself: the ready
/// frontier, when sorted by work order.
#[derive(Clone, Copy, Debug, Default)]
struct RowMarks<'a> {
    /// The cards `headway next --ready` would hand out, by id; `None` when
    /// the listing doesn't mark them.
    ready: Option<&'a HashSet<[u8; 32]>>,
}

impl RowMarks<'_> {
    /// Whether `row` is marked ready.
    fn is_ready(&self, row: &ListRow) -> bool {
        self.ready
            .is_some_and(|ready| ready.contains(row.card.id.bytes()))
    }
}

/// The padded widths of a listing's cells, so every ref and every title
/// starts in one column, and which cells it prints. A marker slot is only
/// there when some row has that marker, so a listing with none starts flush
/// with the ref.
#[derive(Clone, Copy, Debug)]
struct RowWidths {
    /// The properties a row prints.
    display: DisplayProps,
    /// Some row is blocked: every row gets a [`BLOCKED_GLYPH`]-wide slot.
    blocked: bool,
    /// Some row has a priority: every row gets a priority-glyph-wide slot.
    priority: bool,
    /// Some row is ready: every row gets a [`READY_GLYPH`]-wide slot.
    ready: bool,
    reference: usize,
    column: usize,
}

impl RowWidths {
    /// The widths `rows` need to print `display`'s properties. A hidden
    /// property takes no width.
    fn of(rows: &[ListRow], display: DisplayProps, marks: RowMarks) -> RowWidths {
        let mut widths = RowWidths {
            display,
            blocked: false,
            priority: false,
            ready: false,
            reference: 0,
            column: 0,
        };
        for row in rows {
            widths.ready |= marks.is_ready(row);
            widths.blocked |= display.contains(DisplayProp::Blockers) && row.card.is_blocked();
            widths.priority |=
                display.contains(DisplayProp::Priority) && row.card.priority != Priority::None;
            if display.contains(DisplayProp::Id) {
                let reference = plain_ref(row.board, &row.card.id).chars().count();
                widths.reference = widths.reference.max(reference);
            }
            if display.contains(DisplayProp::Status) {
                widths.column = widths.column.max(row.place_name().chars().count() + 2);
            }
        }
        widths
    }
}

/// One row: the ready, blocked and priority markers, the card's full ref (yellow,
/// pasteable), its `[column]` dimmed, then its title in bold with its labels,
/// subissue rollup, estimate, dates and board after it — each only when
/// `widths.display` holds its property. The title is always there.
fn list_line(row: &ListRow, widths: RowWidths, marks: RowMarks, color: bool) -> String {
    let card = row.card;
    let shows = |prop| widths.display.contains(prop);
    let mut line = String::new();
    match (widths.ready, marks.is_ready(row)) {
        (true, true) => line.push_str(&paint(color, SGR_REF, READY_GLYPH)),
        (true, false) => line.push_str(MARKER_GAP),
        (false, _) => {}
    }
    match (widths.blocked, card.is_blocked()) {
        (true, true) => line.push_str(&paint(color, SGR_DIM, BLOCKED_GLYPH)),
        (true, false) => line.push_str(MARKER_GAP),
        (false, _) => {}
    }
    match (widths.priority, card.priority) {
        (false, _) => {}
        (true, Priority::None) => line.push_str(MARKER_GAP),
        // Urgent is the one marker drawn at full strength, as `show` does.
        (true, Priority::Urgent) => line.push_str(priority_glyph(card.priority)),
        (true, p) => line.push_str(&paint(color, SGR_DIM, priority_glyph(p))),
    }
    if shows(DisplayProp::Id) {
        let reference = format!(
            "{:<width$}",
            plain_ref(row.board, &card.id),
            width = widths.reference
        );
        line.push_str(&paint(color, SGR_REF, &reference));
        line.push_str("  ");
    }
    if shows(DisplayProp::Status) {
        let column = format!(
            "{:<width$}",
            format!("[{}]", row.place_name()),
            width = widths.column
        );
        line.push_str(&paint(color, SGR_DIM, &column));
        line.push_str("  ");
    }
    line.push_str(&paint(color, SGR_TITLE, &card.title));
    if shows(DisplayProp::Labels) {
        line.push_str(&labels_suffix(&card.labels));
    }

    let mut dim = |text: &str| {
        line.push_str("  ");
        line.push_str(&paint(color, SGR_DIM, text));
    };
    if shows(DisplayProp::SubIssues) && !card.subissues.is_empty() {
        let done = card.subissues.iter().filter(|s| s.done).count();
        dim(&format!("{done}/{}", card.subissues.len()));
    }
    if shows(DisplayProp::Estimate)
        && let Some(points) = card.estimate
    {
        dim(&format!("{points}pt"));
    }
    if shows(DisplayProp::Due)
        && let Some(due) = card.due
    {
        dim(&format!("due {due}"));
    }
    if shows(DisplayProp::Created) {
        dim(&format!("created {}", rel_time(card.created_at)));
    }
    if shows(DisplayProp::Updated) {
        dim(&format!("updated {}", rel_time(card.updated_at)));
    }
    if shows(DisplayProp::Board) {
        dim(&format!("on {}", row.board.title));
    }
    line.push('\n');
    line
}

/// The blank a row without a marker puts in that marker's slot: as wide as
/// every glyph (one character and a space).
const MARKER_GAP: &str = "  ";

/// The marker on a row in the ready frontier, in a list sorted by work order:
/// what `headway next --ready` would hand out.
const READY_GLYPH: &str = "▶ ";

#[cfg(test)]
mod tests {
    use super::*;

    use headway::event::{CardView, ColumnView};
    use headway::query::Placement;
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
                placement: Placement::Column(column),
            })
            .collect();
        headway::fmt::freeze_now(0);
        let widths = RowWidths::of(&rows, DisplayProps::default(), RowMarks::default());
        let line = list_line(&rows[0], widths, RowMarks::default(), false);
        assert!(!line.contains('\x1b'), "{line:?}");
        assert_eq!(
            line,
            format!(
                "↑ {:<width$}  [Todo]  relay reconnect  [ui]  updated just now\n",
                plain_ref(&view, &rows[0].card.id),
                width = widths.reference,
            )
        );
        // An unprioritised row keeps the slot blank, so the refs line up.
        assert!(
            list_line(&rows[1], widths, RowMarks::default(), false).starts_with("  headway:work/"),
            "{:?}",
            list_line(&rows[1], widths, RowMarks::default(), false)
        );
        // With no priority anywhere there is no slot at all.
        let flush = RowWidths::of(&rows[1..], DisplayProps::default(), RowMarks::default());
        assert!(
            list_line(&rows[1], flush, RowMarks::default(), false).starts_with("headway:work/")
        );
        let json = row_json(&rows[0], RowMarks::default());
        assert_eq!(json["column"], "Todo");
        assert_eq!(json["board"], "work");
        assert_eq!(json["priority"], "high");
    }

    /// Each property prints only while shown: hiding one drops its cell and
    /// its slot, and the title stays whatever is hidden.
    #[test]
    fn display_props_pick_the_cells() {
        headway::fmt::freeze_now(0);
        let mut c = card(1, "relay reconnect");
        c.priority = Priority::High;
        c.labels = vec!["ui".to_string()];
        c.estimate = Some(3);
        c.due = headway::event::Date::parse("2026-10-20");
        let view = board(vec![c]);
        let column = &view.columns[0];
        let row = ListRow {
            board: &view,
            card: &column.cards[0],
            placement: Placement::Column(column),
        };
        let marks = RowMarks::default();
        let line = |display| list_line(&row, RowWidths::of(&[row], display, marks), marks, false);

        let full = line(DisplayProps::default());
        assert!(
            full.ends_with("relay reconnect  [ui]  3pt  due 2026-10-20  updated just now\n"),
            "{full:?}"
        );

        let hide = |names: &str| {
            DisplayProps::shown_hidden(None, DisplayProps::parse_list(names).unwrap())
        };
        let no_labels = line(hide("labels"));
        assert!(!no_labels.contains("[ui]"), "{no_labels:?}");
        assert!(no_labels.contains("3pt"), "{no_labels:?}");
        assert!(!line(hide("priority")).starts_with('↑'));

        let only = DisplayProps::of(&[DisplayProp::Status, DisplayProp::Board]);
        assert_eq!(line(only), "[Todo]  relay reconnect  on Work\n");
        assert_eq!(line(DisplayProps::NONE), "relay reconnect\n");
    }
}
