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
    BuiltinView, DisplayProp, DisplayProps, IssueFilter, ListGroup, ListQuery, ListRow, SortBy,
    ViewSpec, group, list, ready_set,
};
use headway::store::NoPublish;
use headway::views::{SavedView, find_view, new_slug};
use nostrdb::Ndb;
use nostrdb_net::Pubkey;

use crate::args::ListViews;

use crate::grep::{PAGER_VAR, SGR_DIM, SGR_REF, SGR_TITLE};
use crate::output::{BLOCKED_GLYPH, labels_suffix, plain_ref, priority_glyph};

/// How `list`'s rows are written: `--json`, or text under the `--color` and
/// `--pager`/`--no-pager` modes, at most `limit` of them (`-n`). What each
/// row shows, how rows are gathered and their order are the query's.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ListOutput {
    pub(crate) json: bool,
    pub(crate) color: ColorWhen,
    pub(crate) pager: PagerMode,
    pub(crate) limit: Option<usize>,
}

/// `headway list [query...]` — print every live card on `boards` that `query`
/// keeps, one row per card, in `query.sorting`'s order: board order (board,
/// then column, then rank) unless `--sort` says otherwise. `-n` keeps the
/// first rows of that order. Sorted by work order, the rows `headway next
/// --ready` would hand out are marked.
///
/// `query` carries the run's view: its filter ([`ViewSpec::filter`], see
/// [`compose_view`]), its board scope, and `--in <card>`'s subtree, which
/// keeps that card and its descendants.
pub(crate) fn cmd_list(boards: &[BoardView], query: &ListQuery, out: ListOutput) -> Result<()> {
    let mut rows = list(boards, query);
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

/// The view a `list` run shows: `run` — the query words and layout flags
/// the command line gave, its board when `--board` named one — over the view
/// `--view` names, if any. The query words narrow the view's query, a layout
/// flag group that was passed replaces the view's, and `--all` (`include_done`)
/// adds `is:any` when the query says nothing about state. With no `is:` term
/// the list is the open cards, Linear's "Active" ([`ViewSpec::filter`]).
///
/// A saved view is looked up before a built-in one, so a view saved as
/// "Backlog" shadows the built-in.
pub(crate) fn compose_view(
    saved: &[SavedView],
    views: &ListViews,
    run: ViewSpec,
    include_done: bool,
) -> Result<ViewSpec> {
    let mut spec = match &views.apply {
        None => run,
        Some(name) => {
            let base = match find_view(saved, name) {
                Some(view) => view.spec.clone(),
                None => BuiltinView::named(name)
                    .ok_or_else(|| format!("no view named '{name}' (see `headway list --views`)"))?
                    .spec(&run),
            };
            let given = views.given;
            ViewSpec {
                name: base.name,
                query: join_terms(&base.query, &run.query),
                display: if given.display {
                    run.display
                } else {
                    base.display
                },
                grouping: if given.grouping {
                    run.grouping
                } else {
                    base.grouping
                },
                sorting: if given.sorting {
                    run.sorting
                } else {
                    base.sorting
                },
                hide_subissues: base.hide_subissues,
                board: run.board.or(base.board),
            }
        }
    };
    if include_done && IssueFilter::parse(&spec.query, None).state.is_none() {
        spec.query = join_terms(&spec.query, "is:any");
    }
    Ok(spec)
}

/// Two queries' terms as one query: both, space-separated, either may be
/// empty.
fn join_terms(a: &str, b: &str) -> String {
    match (a.trim(), b.trim()) {
        ("", b) => b.to_string(),
        (a, "") => a.to_string(),
        (a, b) => format!("{a} {b}"),
    }
}

/// `headway list --save-view <name>`: save `spec` as `author`'s view `name`,
/// over the view already called that (keeping its slug) or as a new one.
pub(crate) fn save_named_view(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    saved: &[SavedView],
    name: &str,
    spec: ViewSpec,
    json: bool,
) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err("--save-view needs a name".into());
    }
    let slug = match find_view(saved, name) {
        Some(view) => view.slug.clone(),
        None => new_slug(saved, name),
    };
    let view = SavedView {
        slug,
        spec: ViewSpec {
            name: name.to_string(),
            ..spec
        },
    };
    headway::views::save_view(ndb, author, secret, &view, &mut NoPublish);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&view_json(&view.spec, Some(&view.slug)))?
        );
    } else {
        println!("saved view '{}' ({})", view.spec.name, view.slug);
    }
    Ok(())
}

/// `headway list --delete-view <name>`: delete `author`'s saved view `name`.
pub(crate) fn delete_named_view(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    saved: &[SavedView],
    name: &str,
) -> Result<()> {
    let view = find_view(saved, name).ok_or_else(|| format!("no saved view named '{name}'"))?;
    headway::views::delete_view(ndb, author, secret, &view.slug, &mut NoPublish);
    println!("deleted view '{}'", view.spec.name);
    Ok(())
}

/// `headway list --views`: the built-in views, then `saved`, one per line —
/// name, query and any layout off `layout` (the list's defaults) — or as
/// JSON, each with its slug (`null` for a built-in) and whole spec.
pub(crate) fn print_views(saved: &[SavedView], layout: &ViewSpec, json: bool) -> Result<()> {
    let builtins = BuiltinView::ALL.map(|v| v.spec(layout));
    let rows = builtins
        .iter()
        .map(|spec| (spec, None))
        .chain(saved.iter().map(|v| (&v.spec, Some(v.slug.as_str()))));
    if json {
        let all: Vec<serde_json::Value> = rows.map(|(spec, slug)| view_json(spec, slug)).collect();
        println!("{}", serde_json::to_string_pretty(&all)?);
        return Ok(());
    }
    let rows: Vec<(&ViewSpec, Option<&str>)> = rows.collect();
    let width = rows
        .iter()
        .map(|(s, _)| s.name.chars().count())
        .max()
        .unwrap_or(0);
    for (spec, slug) in rows {
        let mut line = format!("{:<width$}  {}", spec.name, spec.query);
        if spec.grouping.is_grouped() {
            line.push_str(&format!("  group:{}", spec.grouping.by));
            if spec.grouping.effective_sub() != headway::query::GroupBy::None {
                line.push_str(&format!("/{}", spec.grouping.sub));
            }
        }
        if spec.sorting != layout.sorting {
            line.push_str(&format!("  sort:{}", spec.sorting.by));
        }
        if let Some(board) = &spec.board {
            line.push_str(&format!("  board:{board}"));
        }
        if slug.is_none() {
            line.push_str("  (built-in)");
        }
        println!("{}", line.trim_end());
    }
    Ok(())
}

/// The `--views --json` shape of one view: its spec's fields, plus its slug
/// and whether it's built in.
fn view_json(spec: &ViewSpec, slug: Option<&str>) -> serde_json::Value {
    let mut value = json!(spec);
    value["slug"] = json!(slug);
    value["builtin"] = json!(slug.is_none());
    value
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
    use headway::query::{Grouping, Placement, Sorting};
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

    /// No `is:` term means open cards; `--all` or any `is:` term overrides
    /// it. `--view` lays its query under the run's words and its layout
    /// under the flags that weren't passed.
    #[test]
    fn compose_view_layers_the_run_over_the_view() {
        use headway::query::{GroupBy, StateFilter};
        let run = |query: &str| ViewSpec {
            query: query.to_string(),
            ..ViewSpec::default()
        };
        let none = ListViews::default();
        let state = |spec: ViewSpec| spec.filter().state;
        let composed = |q: &str, all| compose_view(&[], &none, run(q), all).unwrap();
        assert_eq!(state(composed("relay", false)), Some(StateFilter::Open));
        assert_eq!(state(composed("relay", true)), Some(StateFilter::Any));
        assert_eq!(state(composed("is:done", true)), Some(StateFilter::Done));

        let hot = SavedView {
            slug: "hot".to_string(),
            spec: ViewSpec {
                name: "Hot".to_string(),
                query: "p:high".to_string(),
                grouping: Grouping {
                    by: GroupBy::Label,
                    ..Grouping::default()
                },
                sorting: Sorting::by(SortBy::Due),
                ..ViewSpec::default()
            },
        };
        let mut views = ListViews {
            apply: Some("HOT".to_string()),
            ..ListViews::default()
        };
        views.given.sorting = true;
        let spec =
            compose_view(std::slice::from_ref(&hot), &views, run("label:ui"), false).unwrap();
        assert_eq!(spec.query, "p:high label:ui");
        assert_eq!(
            spec.grouping.by,
            GroupBy::Label,
            "the view's grouping stays"
        );
        assert_eq!(spec.sorting, Sorting::default(), "the passed sort wins");

        views.apply = Some("backlog".to_string());
        let spec = compose_view(&[], &views, run(""), false).unwrap();
        assert_eq!(spec.query, "col:backlog");
        views.apply = Some("nope".to_string());
        assert!(compose_view(&[], &views, run(""), false).is_err());
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
