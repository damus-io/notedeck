//! Listing cards: `nd headway list` and the app's `headway_list` tool.
//!
//! The query is [`crate::query`]'s, the same one the GUI board filter reads,
//! so `p:high label:ui is:done` means one thing everywhere. [`Listing::new`]
//! runs it once; [`Listing::json`] and [`Listing::text`] are the two ways to
//! print it, and [`views_text`] / [`views_json`] print the views, shared by
//! the CLI and the tool so `nd headway list` prints what `nd headway list`
//! prints.

use std::collections::HashSet;

use cli_term::paint;
use nostrdb::Ndb;
use nostrdb_net::Pubkey;
use serde_json::json;

use crate::event::{BoardView, Priority, card_json};
use crate::fmt::rel_time;
use crate::grep::{SGR_DIM, SGR_REF, SGR_TITLE};
use crate::query::{
    BuiltinView, DisplayProp, DisplayProps, GroupBy, IssueFilter, ListGroup, ListQuery, ListRow,
    SortBy, ViewSpec, group, list, ready_set,
};
use crate::render::{BLOCKED_GLYPH, labels_suffix, plain_ref, priority_glyph};
use crate::views::{self, SavedView, find_view, new_slug};

/// What `list` prints when the query keeps no card.
pub const NO_CARDS: &str = "no cards";

/// `list`'s saved-view flags. At most one of `--views`/`--delete-view` makes
/// sense alone; `--view` and `--save-view` combine, saving the applied view
/// with the run's other flags over it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListViews {
    /// `--view <name>`: lay the list out as this saved or built-in view.
    pub apply: Option<String>,
    /// `--save-view <name>`: save the run's query and layout as this view.
    pub save: Option<String>,
    /// `--delete-view <name>`: delete this saved view.
    pub delete: Option<String>,
    /// `--views`: list the views instead of the cards.
    pub list: bool,
    /// Which layout flags the run passed, so they override the applied
    /// view's rather than its being overridden by their defaults.
    pub given: LayoutGiven,
}

/// Which of `list`'s layout flag groups were passed on the command line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutGiven {
    /// `--show`/`--hide`.
    pub display: bool,
    /// `--group`/`--sub-group`/`--empty`.
    pub grouping: bool,
    /// `--sort`/`--descending`/`--ascending`/`--completed-by-recency`.
    pub sorting: bool,
}

/// One `list` run's answer: the rows `query` keeps on the boards it read, in
/// `query.sorting`'s order and capped at `-n`, with the groups they fall in
/// and the ready frontier it marks.
pub struct Listing<'b> {
    rows: Vec<ListRow<'b>>,
    /// The rows' groups, when the query groups them.
    groups: Option<Vec<ListGroup>>,
    /// The cards `nd headway next --ready` would hand out, when sorted by work
    /// order.
    ready: Option<HashSet<[u8; 32]>>,
    display: DisplayProps,
}

impl<'b> Listing<'b> {
    /// Every live card on `boards` that `query` keeps, one row per card, in
    /// `query.sorting`'s order: board order (board, then column, then rank)
    /// unless the query sorts otherwise. `limit` (`-n`) keeps the first rows
    /// of that order. Sorted by work order, the rows `nd headway next --ready`
    /// would hand out are marked.
    ///
    /// `query` carries the run's view: its filter ([`ViewSpec::filter`], see
    /// [`compose_view`]), its board scope, and `--in <card>`'s subtree, which
    /// keeps that card and its descendants.
    pub fn new(boards: &'b [BoardView], query: &ListQuery, limit: Option<usize>) -> Self {
        let mut rows = list(boards, query);
        if let Some(n) = limit {
            rows.truncate(n);
        }
        let ready = (query.sorting.by == SortBy::WorkOrder).then(|| ready_set(&rows));
        let groups = query.grouping.is_grouped().then(|| {
            let boards: Vec<&BoardView> = boards.iter().collect();
            group(&boards, &rows, query.grouping)
        });
        Listing {
            rows,
            groups,
            ready,
            display: query.grouping.row_display(query.display),
        }
    }

    /// What the listing marks on its rows.
    fn marks(&self) -> RowMarks<'_> {
        RowMarks {
            ready: self.ready.as_ref(),
        }
    }

    /// The `list --json` shape: one [`row_json`] per row, or with grouping
    /// one object per group.
    pub fn json(&self) -> serde_json::Value {
        let marks = self.marks();
        match &self.groups {
            Some(groups) => groups_json(&self.rows, groups, marks),
            None => self.rows.iter().map(|row| row_json(row, marks)).collect(),
        }
    }

    /// The text `list` prints, its refs, titles and markers colored when
    /// `color`. Empty when the query kept nothing; the caller says
    /// [`NO_CARDS`].
    pub fn text(&self, color: bool) -> String {
        let marks = self.marks();
        let widths = RowWidths::of(&self.rows, self.display, marks);
        match &self.groups {
            Some(groups) => {
                let mut output = String::new();
                for g in groups {
                    group_lines(&mut output, &self.rows, g, widths, marks, color, "");
                }
                output
            }
            None => self
                .rows
                .iter()
                .map(|row| list_line(row, widths, marks, color))
                .collect(),
        }
    }
}

/// The view a `list` run shows: `run` — the query words and layout flags
/// the command line gave, its board when `--board` named one — over the view
/// `views.apply` names, if any. The query words narrow the view's query, a
/// layout flag group that was passed replaces the view's, and `--all`
/// (`include_done`) adds `is:any` when the query says nothing about state.
/// With no `is:` term the list is the open cards, Linear's "Active"
/// ([`ViewSpec::filter`]).
///
/// A saved view is looked up before a built-in one, so a view saved as
/// "Backlog" shadows the built-in.
pub fn compose_view(
    saved: &[SavedView],
    views: &ListViews,
    run: ViewSpec,
    include_done: bool,
) -> Result<ViewSpec, String> {
    let mut spec = match &views.apply {
        None => run,
        Some(name) => {
            let base = match find_view(saved, name) {
                Some(view) => view.spec.clone(),
                None => BuiltinView::named(name)
                    .ok_or_else(|| {
                        format!("no view named '{name}' (see `nd headway list --views`)")
                    })?
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

/// `list --save-view <name>`: save `spec` as `me`'s view `name`, signed with
/// `secret`, over the view already called that among `saved` (keeping its
/// slug) or as a new one. Answers the view as saved.
pub fn save_named_view(
    ndb: &Ndb,
    me: &Pubkey,
    secret: &[u8; 32],
    saved: &[SavedView],
    name: &str,
    spec: ViewSpec,
) -> Result<SavedView, String> {
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
    views::save_view(ndb, me, secret, &view, 0);
    Ok(view)
}

/// `list --delete-view <name>`: delete `me`'s saved view `name`, signed with
/// `secret`. Answers the view that was deleted.
pub fn delete_named_view(
    ndb: &Ndb,
    me: &Pubkey,
    secret: &[u8; 32],
    saved: &[SavedView],
    name: &str,
) -> Result<SavedView, String> {
    let view = find_view(saved, name).ok_or_else(|| format!("no saved view named '{name}'"))?;
    views::delete_view(ndb, me, secret, &view.slug, 0);
    Ok(view.clone())
}

/// The line `--save-view` prints for `view`.
pub fn saved_view_line(view: &SavedView) -> String {
    format!("saved view '{}' ({})", view.spec.name, view.slug)
}

/// The line `--delete-view` prints for `view`.
pub fn deleted_view_line(view: &SavedView) -> String {
    format!("deleted view '{}'", view.spec.name)
}

/// `builtins`, then `saved`: each spec and its slug (`None` for a built-in).
fn all_views<'s>(
    builtins: &'s [ViewSpec],
    saved: &'s [SavedView],
) -> impl Iterator<Item = ViewEntry<'s>> {
    builtins
        .iter()
        .map(|spec| ViewEntry { spec, slug: None })
        .chain(saved.iter().map(|v| ViewEntry {
            spec: &v.spec,
            slug: Some(v.slug.as_str()),
        }))
}

/// One view `--views` lists: its spec, and its slug when it is saved.
struct ViewEntry<'s> {
    spec: &'s ViewSpec,
    slug: Option<&'s str>,
}

/// `list --views --json`: the built-in views, then `saved`, each its spec
/// with its slug (`null` for a built-in) and whether it's built in.
pub fn views_json(saved: &[SavedView], layout: &ViewSpec) -> serde_json::Value {
    let builtins = BuiltinView::ALL.map(|v| v.spec(layout));
    all_views(&builtins, saved)
        .map(|v| view_json(v.spec, v.slug))
        .collect()
}

/// `list --views`: the built-in views, then `saved`, one per line — name,
/// query and any layout off `layout` (the list's defaults).
pub fn views_text(saved: &[SavedView], layout: &ViewSpec) -> String {
    let builtins = BuiltinView::ALL.map(|v| v.spec(layout));
    let width = all_views(&builtins, saved)
        .map(|v| v.spec.name.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for ViewEntry { spec, slug } in all_views(&builtins, saved) {
        let mut line = format!("{:<width$}  {}", spec.name, spec.query);
        if spec.grouping.is_grouped() {
            line.push_str(&format!("  group:{}", spec.grouping.by));
            if spec.grouping.effective_sub() != GroupBy::None {
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
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
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

/// The `--views --json` shape of one view: its spec's fields, plus its slug
/// and whether it's built in.
pub fn view_json(spec: &ViewSpec, slug: Option<&str>) -> serde_json::Value {
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
    /// The cards `nd headway next --ready` would hand out, by id; `None` when
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
/// what `nd headway next --ready` would hand out.
const READY_GLYPH: &str = "▶ ";

#[cfg(test)]
mod tests {
    use super::*;

    use crate::event::{CardView, ColumnView};
    use crate::query::{Grouping, Placement, Sorting};
    use nostrdb_net::NoteId;

    fn card(n: u8, title: &str) -> CardView {
        CardView {
            id: NoteId::new([n; 32]),
            author: [0; 32],
            title: title.to_string(),
            description: String::new(),
            description_attachments: Vec::new(),
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
        use crate::query::{GroupBy, StateFilter};
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
        crate::fmt::freeze_now(0);
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
        crate::fmt::freeze_now(0);
        let mut c = card(1, "relay reconnect");
        c.priority = Priority::High;
        c.labels = vec!["ui".to_string()];
        c.estimate = Some(3);
        c.due = crate::event::Date::parse("2026-10-20");
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
