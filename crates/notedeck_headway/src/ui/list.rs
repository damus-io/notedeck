//! The issue list: every board's cards as one filterable list, one row per
//! card — the Linear-style lens across issues that the board grid, being one
//! board at a time, can't be (headway:headway/acoustic-canvas-diesel).
//!
//! It reads the same query language as the grid's filter and `headway list`
//! ([`headway::query::IssueFilter`], unscoped, so a pasted ref names exactly
//! one card), with an Active/All toggle standing in for `is:open`. The rows
//! are recomputed only when a board re-folds or the query changes
//! ([`ListState::refresh`]); a steady frame draws the cached rows, and only
//! the ones on screen.
//!
//! A row is Linear's one line (headway:headway/end-kick-draft): priority, ref
//! and status circle, the title, then right-aligned its parent and subissue
//! rollup, labels, estimate, blockers, due date and when it changed — each a
//! [`DisplayProp`] the header's Display menu turns on and off.
//!
//! The rows are gathered under group headers (headway:headway/office-flower-churn),
//! by status unless the Display menu says otherwise, each split again by a
//! sub-grouping if one is set: [`headway::query::group`], the same grouping
//! `headway list --group` prints. A header folds its rows away, and its `+`
//! adds a card already in that group (see [`group_add`]).
//!
//! The rows are ordered before they're grouped
//! (headway:headway/battle-stock-expire), by priority unless the Display menu
//! says otherwise: [`headway::query::sort_order`], what `headway list --sort`
//! prints. Ordered by work order, the rows `headway next --ready` would hand
//! out wear an accent bar, and a row on the board showing drags into a new
//! place in that order (see [`work_order_drop`]).
//!
//! Above the rows sit the view tabs (headway:headway/brave-project-parade):
//! the built-in Active, Backlog and All issues, then the account's saved
//! views ([`headway::views`]). A tab is a [`ViewSpec`] — a query and a
//! layout — and selecting one loads it. The query field narrows the tab's
//! own query rather than showing it; a saved view whose layout or query was
//! changed offers Save, and "+ View" saves what the list shows as a new one.

use nostrdb_net::NoteId;
use notedeck::ColorTheme;
use notedeck::tokens::{RADIUS_MD, SPACING_LG, SPACING_MD, SPACING_SM, SPACING_XS};

use super::widgets::{
    KeyedText, StatusIcon, label_chip, priority_icon_ui, priority_label, status_icon_ui, text_pill,
    tinted_pill,
};
use super::{BoardEffect, BoardNav, BoardUiState, ViewEdit};
use crate::cache::BoardRef;
use crate::cursor::CursorMove;
use crate::event::{self, BoardView, CardView, Priority};
use crate::keys;
use crate::store::{self, BoardAction, NewCard};
use headway::query::{
    BuiltinView, DisplayProp, DisplayProps, GroupBy, GroupKey, Grouping, ListGroup, ListQuery,
    ListRow, RowSlot, SortBy, Sorting, ViewSpec,
};
use headway::views::{SavedView, new_slug};

/// Height of one list row, fixed so the scroll area lays out only the rows on
/// screen ([`egui::ScrollArea::show_rows`]).
const ROW_HEIGHT: f32 = 32.0;

/// The issue list's view state: whether it shows, what it's asked, its rows
/// and its keyboard cursor. Kept across frames on
/// [`BoardUiState`](super::BoardUiState), so leaving the list and coming
/// back (a back off a card opened from it) finds it as it was left.
pub(crate) struct ListState {
    /// Whether the list shows, seeded from the nav route each frame like the
    /// graph's epic (see [`BoardUiState::set_list_open`]).
    open: bool,
    /// The query field's text: terms that narrow the tab's own query
    /// ([`base`](Self::base)), parsed together with it by
    /// [`IssueFilter::parse`](headway::query::IssueFilter::parse).
    query: String,
    /// The tab showing: a built-in view or a saved one.
    tab: ListTab,
    /// The tab's own query, loaded with it. Without an `is:` term, it and
    /// the field together keep open cards ([`ViewSpec::filter`]).
    base: String,
    /// Drop subissues, keeping top-level cards: the Display menu's
    /// "Sub-issues" toggle, off.
    hide_subissues: bool,
    /// The board slug the tab's view is scoped to; `None` lists every board.
    board_scope: Option<String>,
    /// The account's saved views, in slug order: the tabs after the
    /// built-ins. Loaded by the app on an account switch
    /// ([`BoardUiState::set_saved_views`]) and kept in step with the edits
    /// the tabs raise, so a save shows before its note is ingested.
    saved: Vec<SavedView>,
    /// The view-name field, while one is open: naming a new view, or
    /// renaming a saved one.
    naming: Option<ViewNaming>,
    /// The card under the keyboard cursor. A card rather than a row index,
    /// so a re-fold that reorders the rows leaves it on the same card.
    cursor: Option<NoteId>,
    /// Scroll the cursor's row into view on the next draw.
    scroll_to_cursor: bool,
    /// The rows the query keeps, in board order. Valid for
    /// [`seen`](Self::seen)'s sources.
    rows: Vec<ListLine>,
    /// What [`rows`](Self::rows) was computed from.
    seen: Option<ListInputs>,
    /// The header's `"N issues"` count.
    count: KeyedText<usize>,
    /// Which properties each row shows beside its title, toggled in the
    /// header's Display menu; a saved view keeps it.
    display: DisplayProps,
    /// How the rows are gathered under headers, set in the Display menu.
    /// By status unless changed, as Linear's list is.
    grouping: Grouping,
    /// How the rows are ordered, set in the Display menu. By priority unless
    /// changed, as Linear's list is.
    sorting: Sorting,
    /// The groups [`rows`](Self::rows) fall in, their rows as indices into
    /// it. One [`GroupKey::All`] group while the list isn't grouped.
    groups: Vec<ListGroup>,
    /// The headers folded shut. Kept across refreshes, by group, so a
    /// folded group stays folded while its rows change.
    collapsed: std::collections::HashSet<CollapseKey>,
    /// What the list draws, top to bottom: headers and the rows of the
    /// groups that are open. Rebuilt from [`groups`](Self::groups) when they
    /// change or a header folds, never per frame.
    entries: Vec<ListEntry>,
    /// The `+` composer, while one is open: a card for a group.
    compose: Option<GroupCompose>,
}

impl Default for ListState {
    fn default() -> ListState {
        ListState {
            open: false,
            query: String::new(),
            tab: ListTab::Builtin(BuiltinView::Active),
            base: BuiltinView::Active.query().to_string(),
            hide_subissues: false,
            board_scope: None,
            saved: Vec::new(),
            naming: None,
            cursor: None,
            scroll_to_cursor: false,
            rows: Vec::new(),
            seen: None,
            count: KeyedText::default(),
            display: DisplayProps::default(),
            grouping: Grouping {
                by: GroupBy::Status,
                ..Grouping::default()
            },
            sorting: Sorting::by(SortBy::Priority),
            groups: Vec::new(),
            collapsed: std::collections::HashSet::new(),
            entries: Vec::new(),
            compose: None,
        }
    }
}

/// Which view tab is showing.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ListTab {
    /// A built-in view.
    Builtin(BuiltinView),
    /// The saved view with this slug.
    Saved(String),
}

/// The view-name field: what it names, and the name so far.
struct ViewNaming {
    /// The saved view it renames, by slug; `None` names a new view of what
    /// the list shows.
    rename: Option<String>,
    /// The name typed so far.
    name: String,
    /// Focus the field on its next draw.
    focus: bool,
}

/// Which header a fold or a `+` is for: a group, or a sub-group within one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CollapseKey {
    /// The top-level group.
    group: GroupKey,
    /// The sub-group within it, for a sub-group's header.
    sub: Option<GroupKey>,
}

/// One line the list draws: a group's header, or a card's row.
enum ListEntry {
    /// A group's header.
    Header(GroupHeader),
    /// The row at this index into [`ListState::rows`].
    Row(usize),
}

/// A group header's text and state, made when the entries are rebuilt so a
/// frame only draws it.
struct GroupHeader {
    /// Which group it heads.
    key: CollapseKey,
    /// The group's name.
    label: String,
    /// How many rows it holds.
    count: String,
    /// Whether it's folded shut.
    collapsed: bool,
}

impl GroupHeader {
    /// The header for group `g`, under `key`.
    fn of(key: CollapseKey, g: &ListGroup, collapsed: bool) -> GroupHeader {
        GroupHeader {
            key,
            label: g.label.clone(),
            count: g.rows.len().to_string(),
            collapsed,
        }
    }
}

/// The `+` composer: the group a new card goes in, and its title so far.
struct GroupCompose {
    /// The group, as its header names it.
    key: CollapseKey,
    /// The group's name, for the field's hint.
    label: String,
    /// The title typed so far.
    title: String,
    /// Focus the field on its next draw.
    focus: bool,
}

/// One row of the list: where its card sits in the sources it was computed
/// from, by index, and the text a row draws that would otherwise be formatted
/// every frame.
struct ListLine {
    /// Index of the card's board in the sources.
    board: usize,
    /// Where the card sits on that board.
    slot: RowSlot,
    /// The card's id.
    id: NoteId,
    /// The card's word-id (`maple-river-canyon`).
    word_id: String,
    /// The card is in its board's ready frontier and the list is ordered by
    /// work order: the row wears the accent bar.
    ready: bool,
    /// The text of the row's properties, formatted once when the rows are
    /// computed rather than every frame.
    text: LineText,
}

/// A [`ListLine`]'s formatted property text. `None` where the card has no
/// value for that property, so its cell is skipped rather than drawn empty.
struct LineText {
    /// `› <parent title>`, cut short, when the card is a subissue of a card
    /// on its board.
    parent: Option<String>,
    /// `done/total` of its subissues.
    subissues: Option<String>,
    /// `3 pt`.
    estimate: Option<String>,
    /// `⊘ 2`: how many cards block it.
    blockers: Option<String>,
    /// `Oct 20`.
    due: Option<String>,
    /// When it was created, relative (`3d ago`). Relative to when the rows
    /// were computed: a steady list doesn't re-age.
    created: String,
    /// When its content last changed, relative, likewise.
    updated: String,
}

impl LineText {
    /// The property text for `card` on `board`.
    fn of(board: &BoardView, card: &CardView) -> LineText {
        let parent = card
            .parent
            .and_then(|id| board.card(id))
            .map(|parent| format!("› {}", clip(&parent.title, PARENT_CHARS)));
        let subissues = (!card.subissues.is_empty()).then(|| {
            let done = card.subissues.iter().filter(|s| s.done).count();
            format!("{done}/{}", card.subissues.len())
        });
        let blockers =
            (!card.blocked_by.is_empty()).then(|| format!("⊘ {}", card.blocked_by.len()));
        LineText {
            parent,
            subissues,
            estimate: card.estimate.map(|points| format!("{points} pt")),
            blockers,
            due: card.due.map(short_date),
            created: headway::fmt::rel_time(card.created_at),
            updated: headway::fmt::rel_time(card.updated_at),
        }
    }
}

/// How many characters of a parent's title a row's breadcrumb keeps.
const PARENT_CHARS: usize = 28;

/// `text`, cut to `max` characters with an ellipsis when it's longer.
fn clip(text: &str, max: usize) -> std::borrow::Cow<'_, str> {
    match text.char_indices().nth(max) {
        None => std::borrow::Cow::Borrowed(text),
        Some((at, _)) => std::borrow::Cow::Owned(format!("{}…", text[..at].trim_end())),
    }
}

/// A due date the way Linear's row shows one: `Oct 20`.
fn short_date(date: event::Date) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = MONTHS[usize::from(date.month.clamp(1, 12)) - 1];
    format!("{month} {}", date.day)
}

/// The status icon for where `slot` sits on `board`: filled in a terminal
/// column, half in In Progress, and otherwise by position, as the grid's
/// column headers draw it.
fn slot_icon(slot: RowSlot, board: &BoardView) -> StatusIcon {
    let RowSlot::Column { col, .. } = slot else {
        return StatusIcon::Archived;
    };
    let column = &board.columns[col];
    if board.column_is_terminal(&column.id) {
        StatusIcon::Done
    } else if column.is_in_progress() {
        StatusIcon::Started(0.5)
    } else {
        StatusIcon::for_column(col, board.columns.len())
    }
}

/// The name of where `slot` sits on `board`: its column's, or "Archived".
fn slot_place(slot: RowSlot, board: &BoardView) -> &str {
    match slot {
        RowSlot::Column { col, .. } => &board.columns[col].name,
        RowSlot::Archived(_) => "Archived",
    }
}

/// The inputs the cached rows were computed from: when any differs, the rows
/// are stale.
struct ListInputs {
    /// Each board read, as a handle into its fold; compared by pointer.
    sources: Vec<BoardRef>,
    /// The query text.
    query: String,
    /// The tab's own query.
    base: String,
    /// The sub-issue toggle.
    hide_subissues: bool,
    /// The tab's board scope.
    board_scope: Option<String>,
    /// The grouping.
    grouping: Grouping,
    /// The ordering.
    sorting: Sorting,
}

/// Where a list row opens: its card, and the board it's on.
struct RowTarget {
    card: NoteId,
    board: event::BoardCoord,
}

impl ListState {
    /// Whether the list shows.
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// The card under the keyboard cursor, if any.
    pub(crate) fn cursor(&self) -> Option<NoteId> {
        self.cursor
    }

    /// Show or hide the list.
    pub(super) fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    /// Whether the cached rows were computed from other inputs than these.
    /// Compares the boards by pointer and the query by value, so a steady
    /// frame allocates nothing to find out.
    fn stale(&self, sources: &[BoardRef]) -> bool {
        let Some(seen) = &self.seen else {
            return true;
        };
        seen.hide_subissues != self.hide_subissues
            || seen.base != self.base
            || seen.board_scope != self.board_scope
            || seen.grouping != self.grouping
            || seen.sorting != self.sorting
            || seen.query != self.query
            || seen.sources.len() != sources.len()
            || seen
                .sources
                .iter()
                .zip(sources)
                .any(|(was, now)| !was.same_fold(now))
    }

    /// Recompute the rows, their order and their groups from `sources` if
    /// they, the query, the toggle, the grouping or the ordering changed since
    /// the last time.
    fn refresh(&mut self, sources: &[BoardRef]) {
        if !self.stale(sources) {
            return;
        }
        let spec = self.spec();
        let query = ListQuery {
            filter: spec.filter(),
            board: spec.board,
            sorting: self.sorting,
            ..ListQuery::default()
        };
        let positions = headway::query::list_positions(sources.iter().map(|b| &**b), &query);
        // The kept rows as `headway::query` reads them, index for index with
        // `self.rows`, to mark and group.
        let kept: Vec<ListRow> = positions
            .iter()
            .map(|pos| pos.row(&sources[pos.board]))
            .collect();
        // Work order marks what `headway next --ready` would hand out.
        let ready = if self.sorting.by == SortBy::WorkOrder {
            headway::query::ready_set(&kept)
        } else {
            Default::default()
        };
        self.rows.clear();
        self.rows
            .extend(positions.iter().zip(&kept).map(|(pos, row)| ListLine {
                board: pos.board,
                slot: pos.slot,
                id: row.card.id,
                word_id: headway::wordid::encode(row.card.id.bytes()),
                ready: ready.contains(row.card.id.bytes()),
                text: LineText::of(row.board, row.card),
            }));
        let boards: Vec<&BoardView> = sources.iter().map(|b| &**b).collect();
        self.groups = headway::query::group(&boards, &kept, self.grouping);
        self.rebuild_entries();
        self.seen = Some(ListInputs {
            sources: sources.to_vec(),
            query: self.query.clone(),
            base: self.base.clone(),
            hide_subissues: self.hide_subissues,
            board_scope: self.board_scope.clone(),
            grouping: self.grouping,
            sorting: self.sorting,
        });
    }

    /// Whether a row can be dragged to a new place in its board's work
    /// order: the list runs in work order, top first, and the row's card is
    /// live on `view`, the board an edit applies to.
    fn can_drag(&self, sources: &[BoardRef], line: &ListLine, view: &BoardView) -> bool {
        if self.sorting.by != SortBy::WorkOrder || self.sorting.descending {
            return false;
        }
        let board: &BoardView = &sources[line.board];
        matches!(line.slot, RowSlot::Column { .. })
            && board.author == view.author
            && board.id == view.id
    }

    /// Lay [`groups`](Self::groups) out as the lines the list draws: each
    /// header, then its rows or its sub-groups, skipping what's folded. An
    /// ungrouped list is its rows alone.
    fn rebuild_entries(&mut self) {
        self.entries.clear();
        if !self.grouping.is_grouped() {
            self.entries
                .extend((0..self.rows.len()).map(ListEntry::Row));
            return;
        }
        for g in &self.groups {
            let key = CollapseKey {
                group: g.key.clone(),
                sub: None,
            };
            let collapsed = self.collapsed.contains(&key);
            self.entries
                .push(ListEntry::Header(GroupHeader::of(key, g, collapsed)));
            if collapsed {
                continue;
            }
            if g.subgroups.is_empty() {
                self.entries
                    .extend(g.rows.iter().copied().map(ListEntry::Row));
                continue;
            }
            for sub in &g.subgroups {
                let key = CollapseKey {
                    group: g.key.clone(),
                    sub: Some(sub.key.clone()),
                };
                let collapsed = self.collapsed.contains(&key);
                self.entries
                    .push(ListEntry::Header(GroupHeader::of(key, sub, collapsed)));
                if !collapsed {
                    self.entries
                        .extend(sub.rows.iter().copied().map(ListEntry::Row));
                }
            }
        }
    }

    /// Fold the header `key` shut, or open it again.
    fn toggle_collapsed(&mut self, key: &CollapseKey) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.clone());
        }
        self.rebuild_entries();
    }

    /// What the list shows, as a view: the tab's name, its query narrowed
    /// by the field's, and the layout the Display menu holds. Its
    /// [`filter`](ViewSpec::filter) keeps open cards when neither query
    /// names a state, the default `headway list` takes.
    fn spec(&self) -> ViewSpec {
        let query = match (self.base.trim(), self.query.trim()) {
            (base, "") => base.to_string(),
            ("", query) => query.to_string(),
            (base, query) => format!("{base} {query}"),
        };
        ViewSpec {
            name: self.tab_name().to_string(),
            query,
            grouping: self.grouping,
            sorting: self.sorting,
            display: self.display,
            hide_subissues: self.hide_subissues,
            board: self.board_scope.clone(),
        }
    }

    /// The layout a built-in tab loads: the list's defaults.
    fn default_layout() -> ViewSpec {
        let defaults = ListState::default();
        ViewSpec {
            grouping: defaults.grouping,
            sorting: defaults.sorting,
            display: defaults.display,
            ..ViewSpec::default()
        }
    }

    /// The saved view `slug`, if the account has it.
    fn saved_view(&self, slug: &str) -> Option<&SavedView> {
        self.saved.iter().find(|v| v.slug == slug)
    }

    /// The showing tab's name.
    fn tab_name(&self) -> &str {
        match &self.tab {
            ListTab::Builtin(builtin) => builtin.name(),
            ListTab::Saved(slug) => self.saved_view(slug).map_or("", |v| v.spec.name.as_str()),
        }
    }

    /// Show `tab`, laid out as its view is: its query under an emptied
    /// field, its layout in the Display menu. Re-picking the tab showing
    /// keeps what was changed on it.
    fn select_tab(&mut self, tab: ListTab) {
        if tab == self.tab {
            return;
        }
        let spec = match &tab {
            ListTab::Builtin(builtin) => builtin.spec(&ListState::default_layout()),
            ListTab::Saved(slug) => match self.saved_view(slug) {
                Some(view) => view.spec.clone(),
                None => return,
            },
        };
        self.tab = tab;
        self.load(spec);
    }

    /// Lay the list out as `spec`, its query as the tab's own.
    fn load(&mut self, spec: ViewSpec) {
        self.base = spec.query;
        self.query.clear();
        self.grouping = spec.grouping;
        self.sorting = spec.sorting;
        self.display = spec.display;
        self.hide_subissues = spec.hide_subissues;
        self.board_scope = spec.board;
        self.cursor = None;
    }

    /// Whether the showing tab is a saved view the list no longer matches:
    /// its query narrowed, or its layout changed. Allocates nothing; the
    /// header asks every frame.
    fn dirty(&self) -> bool {
        let ListTab::Saved(slug) = &self.tab else {
            return false;
        };
        let Some(view) = self.saved_view(slug) else {
            return false;
        };
        let spec = &view.spec;
        !self.query.trim().is_empty()
            || spec.query != self.base
            || spec.grouping != self.grouping
            || spec.sorting != self.sorting
            || spec.display != self.display
            || spec.hide_subissues != self.hide_subissues
    }

    /// Replace the saved views with `views`, as loaded for a newly selected
    /// account. A saved tab the account doesn't have falls back to Active.
    fn set_saved(&mut self, views: Vec<SavedView>) {
        self.saved = views;
        if let ListTab::Saved(slug) = &self.tab
            && self.saved_view(slug).is_none()
        {
            self.select_tab(ListTab::Builtin(BuiltinView::Active));
        }
    }

    /// Keep `view` among the saved views, replacing the one with its slug,
    /// in slug order as [`headway::views::load_views`] reads them.
    fn keep_saved(&mut self, view: SavedView) {
        match self.saved.binary_search_by(|v| v.slug.cmp(&view.slug)) {
            Ok(at) => self.saved[at] = view,
            Err(at) => self.saved.insert(at, view),
        }
    }

    /// Save what the list shows as a new view named `name`, and show its
    /// tab. Returns the edit to persist.
    fn save_new_view(&mut self, name: &str) -> ViewEdit {
        let spec = ViewSpec {
            name: name.to_string(),
            ..self.spec()
        };
        let view = SavedView {
            slug: new_slug(&self.saved, name),
            spec,
        };
        self.tab = ListTab::Saved(view.slug.clone());
        self.base = view.spec.query.clone();
        self.query.clear();
        self.keep_saved(view.clone());
        ViewEdit::Save(view)
    }

    /// Save what the list shows over the saved view showing (the dirty
    /// tab's Save). `None` on a built-in tab.
    fn save_over_tab(&mut self) -> Option<ViewEdit> {
        let ListTab::Saved(slug) = &self.tab else {
            return None;
        };
        let view = SavedView {
            slug: slug.clone(),
            spec: self.spec(),
        };
        self.base = view.spec.query.clone();
        self.query.clear();
        self.keep_saved(view.clone());
        Some(ViewEdit::Save(view))
    }

    /// Rename saved view `slug` to `name`, keeping its slug.
    fn rename_view(&mut self, slug: &str, name: &str) -> Option<ViewEdit> {
        let mut view = self.saved_view(slug)?.clone();
        view.spec.name = name.to_string();
        self.keep_saved(view.clone());
        Some(ViewEdit::Save(view))
    }

    /// Delete saved view `slug`, leaving for Active if it was showing.
    fn delete_view(&mut self, slug: &str) -> ViewEdit {
        self.saved.retain(|v| v.slug != slug);
        if matches!(&self.tab, ListTab::Saved(s) if s == slug) {
            self.select_tab(ListTab::Builtin(BuiltinView::Active));
        }
        ViewEdit::Delete(slug.to_string())
    }

    /// The row (an index into [`rows`](Self::rows)) entry `at` draws, if
    /// it's a row rather than a header.
    fn entry_row(&self, at: usize) -> Option<usize> {
        match self.entries.get(at)? {
            ListEntry::Row(row) => Some(*row),
            ListEntry::Header(_) => None,
        }
    }

    /// The entry of the cursor's row, if the cursor is on a row the list
    /// shows: the first, for a card listed under two labels.
    fn cursor_entry(&self) -> Option<usize> {
        let cursor = self.cursor?;
        (0..self.entries.len()).find(|&at| {
            self.entry_row(at)
                .is_some_and(|row| self.rows[row].id == cursor)
        })
    }

    /// Move the cursor a row down or up, or to the first or last row,
    /// stepping over headers. With no cursor (or one on a card the list no
    /// longer shows), any move lands on the first row. Left and right mean
    /// nothing in one list.
    pub(crate) fn move_cursor(&mut self, mv: CursorMove) {
        let is_row = |at: &usize| self.entry_row(*at).is_some();
        let mut entries = 0..self.entries.len();
        let to = match (self.cursor_entry(), mv) {
            (None, _) | (_, CursorMove::First) => entries.find(is_row),
            (_, CursorMove::Last) => entries.rev().find(is_row),
            (Some(at), CursorMove::Down) => (at + 1..self.entries.len()).find(is_row),
            (Some(at), CursorMove::Up) => (0..at).rev().find(is_row),
            (Some(_), CursorMove::Left | CursorMove::Right) => return,
        };
        let Some(row) = to.and_then(|at| self.entry_row(at)) else {
            return;
        };
        self.cursor = Some(self.rows[row].id);
        self.scroll_to_cursor = true;
    }

    /// Where entry `at`'s row opens, read from the `sources` the rows came
    /// from. `None` for a header.
    fn target(&self, sources: &[BoardRef], at: usize) -> Option<RowTarget> {
        let line = self.rows.get(self.entry_row(at)?)?;
        let board = sources.get(line.board)?;
        Some(RowTarget {
            card: line.id,
            board: event::BoardCoord::new(board.author, board.id.clone()),
        })
    }
}

impl BoardUiState {
    /// Open the issue list over the grid.
    pub(crate) fn open_list(&mut self) {
        self.list.set_open(true);
    }

    /// Leave the issue list for the grid.
    pub(crate) fn close_list(&mut self) {
        self.list.set_open(false);
    }

    /// Seed whether the issue list shows from the chrome global-history route
    /// this frame renders, the list counterpart to
    /// [`set_graph_epic`](Self::set_graph_epic). What the list holds — its
    /// query, toggle and cursor — is left alone, so back/forward onto its
    /// entry finds it as it was left.
    pub fn set_list_open(&mut self, open: bool) {
        self.list.set_open(open);
    }

    /// Open the card under the list's cursor, if there is one.
    pub(crate) fn open_list_cursor(&mut self, view: &BoardView, sources: &[BoardRef]) {
        if let Some(at) = self.list.cursor_entry() {
            self.open_list_row(view, sources, at);
        }
    }

    /// Whether the list's `+` composer or view-name field is open: Esc
    /// closes it rather than the list.
    pub(crate) fn list_composing(&self) -> bool {
        self.list.is_open() && (self.list.compose.is_some() || self.list.naming.is_some())
    }

    /// The account's saved list views, as the app loaded them for a newly
    /// selected account: the tabs after the built-in ones.
    pub fn set_saved_views(&mut self, views: Vec<SavedView>) {
        self.list.set_saved(views);
    }

    /// Open entry `at`'s card: its detail, on top of the list, switching to its
    /// board first when that isn't the board showing. The list stays open
    /// beneath, so leaving the card returns to it.
    fn open_list_row(&mut self, view: &BoardView, sources: &[BoardRef], at: usize) {
        let Some(target) = self.list.target(sources, at) else {
            return;
        };
        if target.board.owner != view.author || target.board.slug != view.id {
            self.raise(BoardEffect::Nav(BoardNav::Switch(target.board)));
        }
        self.list.cursor = Some(target.card);
        self.selected = Some(target.card);
    }
}

/// The issue list's query field's pinned id, shared by the field and the `/`
/// key that focuses it.
pub(crate) fn list_field_id() -> egui::Id {
    egui::Id::unique("headway-list-filter-field")
}

/// Draw the issue list over every board in `sources`, the boards the switcher
/// lists. `view` is the board showing, which a row on another board switches
/// away from when opened, and which a group's `+` adds its card to. Runs the
/// list's UI tools first, so a key that opens a card or leaves the list lands
/// this frame. Returns the card a group's composer adds, if one does.
pub(super) fn list_view_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    sources: &[BoardRef],
    state: &mut BoardUiState,
) -> Option<BoardAction> {
    state.list.refresh(sources);
    while state.list.is_open() && state.selected.is_none() {
        let Some(tool) = state.next_queued_tool() else {
            break;
        };
        keys::apply_list_tool(ui.ctx(), view, sources, state, &tool);
    }

    let mut rows = RowsResponse::default();
    let mut action = None;
    egui::Frame::new()
        .inner_margin(egui::Margin::same(SPACING_LG as i8))
        .show(ui, |ui| {
            list_header_ui(ui, theme, state);
            action = compose_ui(ui, theme, view, &mut state.list);
            // A query edited this frame narrows the rows this frame.
            state.list.refresh(sources);
            ui.add_space(SPACING_SM);
            ui.separator();
            ui.add_space(SPACING_SM);
            rows = list_rows_ui(ui, theme, view, sources, &state.list);
        });
    state.list.scroll_to_cursor = false;

    if let Some(key) = rows.toggle {
        state.list.toggle_collapsed(&key);
    }
    if let Some(key) = rows.add {
        state.list.open_compose(key);
    }
    if let Some(at) = rows.clicked {
        state.open_list_row(view, sources, at);
    }
    if let Some(drop) = rows.dropped {
        action = action.or_else(|| work_order_drop(view, drop));
    }
    action
}

/// A row let go over another in work-order mode: the card dragged, the card
/// it was dropped on, and on which half.
#[derive(Clone, Copy, Debug)]
struct RowDrop {
    /// The card dragged.
    card: NoteId,
    /// The card under the pointer when it was let go.
    onto: NoteId,
    /// Let go on the lower half of that row: the card goes after it.
    below: bool,
}

/// The work-order edit a [`RowDrop`] asks for on `view`, or `None` when it
/// asks for none (dropped on itself or its own subtree, or where it already
/// sits).
///
/// A card moves among the members of its own container: the board's
/// top-level cards, or its parent's subissues. So the drop lands beside
/// whichever of `onto`'s ancestors (or `onto` itself) is its sibling there —
/// a top-level card dropped on a subissue goes beside that subissue's epic.
/// The edit is the shared [`store::reorder`], the same the card detail's
/// subissue drag makes.
fn work_order_drop(view: &BoardView, drop: RowDrop) -> Option<BoardAction> {
    let parent = view.card(drop.card)?.parent;
    let mut sibling = view.card(drop.onto)?;
    // Bounded, so a parent cycle that slipped past the write-time guard
    // can't spin here.
    for _ in 0..64 {
        if sibling.parent == parent {
            break;
        }
        sibling = view.card(sibling.parent?)?;
    }
    if sibling.parent != parent || sibling.id == drop.card {
        return None;
    }
    let container = match parent {
        Some(parent) => event::Container::Card(*parent.bytes()),
        None => event::Container::BoardRoot(view.id.clone()),
    };
    let before = if drop.below {
        let members = headway::traversal::members(view, &container);
        let at = members.iter().position(|&id| id == sibling.id)?;
        members[at + 1..]
            .iter()
            .copied()
            .find(|&id| id != drop.card)
    } else {
        Some(sibling.id)
    };
    store::reorder(view, &container, drop.card, before)
}

/// The payload a work-order row drag carries: the dragged card.
#[derive(Clone, Copy, Debug)]
struct DragRow(NoteId);

/// What the rows were asked this frame: a row clicked, a header folded, a
/// header's `+` pressed.
#[derive(Default)]
struct RowsResponse {
    /// The entry whose row was clicked.
    clicked: Option<usize>,
    /// The header clicked, to fold or unfold.
    toggle: Option<CollapseKey>,
    /// The header whose `+` was pressed.
    add: Option<CollapseKey>,
    /// A work-order drag let go over a row.
    dropped: Option<RowDrop>,
}

/// What a card added under a group's header gets, so it lands in that group:
/// read from the header's group and sub-group against the board showing.
struct GroupAdd<'a> {
    /// The column, on the board showing.
    col: usize,
    /// A label (lowercased, as the group's key holds it).
    label: Option<&'a str>,
    /// A parent on the board showing.
    parent: Option<NoteId>,
    /// A priority ([`Priority::None`] for the no-priority group).
    priority: Priority,
}

/// What a card added under header `key` needs to land in its group on
/// `view`, the board it's added to; `None` when no card added there can
/// (another board's group, an archived group, a column or parent `view`
/// lacks), and the header draws no `+`. Allocates nothing: it runs for every
/// header on screen.
fn group_add<'a>(key: &'a CollapseKey, view: &BoardView) -> Option<GroupAdd<'a>> {
    let mut add = GroupAdd {
        col: 0,
        label: None,
        parent: None,
        priority: Priority::None,
    };
    for group in std::iter::once(&key.group).chain(key.sub.as_ref()) {
        match group {
            GroupKey::All | GroupKey::Label(None) | GroupKey::Parent(None) => {}
            GroupKey::Archived => return None,
            GroupKey::Status(name) => {
                add.col = view
                    .columns
                    .iter()
                    .position(|c| c.name.chars().flat_map(char::to_lowercase).eq(name.chars()))?;
            }
            GroupKey::Priority(p) => add.priority = *p,
            GroupKey::Label(Some(label)) => add.label = Some(label),
            GroupKey::Board { author, id } => {
                if *author != view.author || *id != view.id {
                    return None;
                }
            }
            GroupKey::Parent(Some(parent)) => {
                view.card(*parent)?;
                add.parent = Some(*parent);
            }
        }
    }
    Some(add)
}

impl ListState {
    /// Open the composer for a card in header `key`'s group.
    fn open_compose(&mut self, key: CollapseKey) {
        let label = self
            .entries
            .iter()
            .find_map(|entry| match entry {
                ListEntry::Header(h) if h.key == key => Some(h.label.clone()),
                _ => None,
            })
            .unwrap_or_default();
        self.compose = Some(GroupCompose {
            key,
            label,
            title: String::new(),
            focus: true,
        });
    }
}

/// The `+` composer, while one is open: a title field for a card in its
/// group. Enter adds the card to `view` and keeps the field open for the
/// next; Enter on an empty field, or Esc, closes it. Returns the add.
fn compose_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    list: &mut ListState,
) -> Option<BoardAction> {
    let compose = list.compose.as_mut()?;
    let mut close = false;
    let mut submit = false;
    ui.add_space(SPACING_SM);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("New issue in").color(theme.text_muted));
        ui.label(egui::RichText::new(&compose.label).strong());
        let field = egui::TextEdit::singleline(&mut compose.title)
            .hint_text("Issue title…")
            .desired_width(420.0);
        let response = ui.add(field);
        if std::mem::take(&mut compose.focus) {
            response.request_focus();
        }
        let (enter, escape) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        // Any Esc while it's open: the list's Back stands down for it
        // (`BoardUiState::list_composing`).
        if escape {
            close = true;
        } else if enter && response.lost_focus() {
            submit = true;
        }
        if ui.button("Add").clicked() {
            submit = true;
        }
    });
    if close {
        list.compose = None;
        return None;
    }
    if !submit {
        return None;
    }
    let title = compose.title.trim().to_string();
    if title.is_empty() {
        list.compose = None;
        return None;
    }
    let Some(add) = group_add(&compose.key, view) else {
        // The group no longer takes a card here (the board switched).
        list.compose = None;
        return None;
    };
    let action = BoardAction::AddCard(NewCard {
        col: add.col,
        title,
        labels: add.label.map(str::to_string).into_iter().collect(),
        parent: add.parent,
        priority: add.priority,
        ..NewCard::default()
    });
    compose.title.clear();
    compose.focus = true;
    Some(action)
}

/// The list's header: a way back to the board, the count, the view tabs and
/// their Save, the Display menu and the query field. A tab edit is raised for
/// the app to persist ([`BoardEffect::View`]).
fn list_header_ui(ui: &mut egui::Ui, theme: &ColorTheme, state: &mut BoardUiState) {
    let list = &mut state.list;
    let mut close = false;
    let mut menu_open = false;
    let mut edit = None;
    ui.horizontal(|ui| {
        let back =
            egui::Button::new(egui::RichText::new("← Board").color(theme.text_muted)).frame(false);
        if ui.add(back).clicked() {
            close = true;
        }
        ui.add_space(SPACING_SM);
        ui.heading("Issues");
        ui.add_space(SPACING_SM);
        let n = list.rows.len();
        let count = list.count.text(n, |text, n| {
            use std::fmt::Write;
            let _ = write!(text, "{n} issue{}", if n == 1 { "" } else { "s" });
        });
        ui.label(egui::RichText::new(count).color(theme.text_muted));
        ui.add_space(SPACING_MD);
        edit = view_tabs_ui(ui, theme, list);
        ui.add_space(SPACING_SM);
        menu_open = display_menu_ui(
            ui,
            theme,
            DisplayMenu {
                display: &mut list.display,
                grouping: &mut list.grouping,
                sorting: &mut list.sorting,
                hide_subissues: &mut list.hide_subissues,
            },
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let field = egui::TextEdit::singleline(&mut list.query)
                .id(list_field_id())
                .desired_width(260.0)
                .hint_text("Filter… e.g. p:high label:ui is:done");
            ui.add(field);
        });
    });
    // Open this frame: hold the list keys off it (see `grid_menu_open`).
    state.grid_menu_open |= menu_open;
    if let Some(edit) = edit {
        state.raise(BoardEffect::View(edit));
    }
    if close {
        state.close_list();
    }
}

/// The view tabs: the built-ins, then each saved view (right-click to rename
/// or delete it), then Save while a saved tab is [dirty](ListState::dirty)
/// and "+ View" to save the list as a new one — or, while one is open, the
/// view-name field. Returns the edit a save, rename or delete made.
fn view_tabs_ui(ui: &mut egui::Ui, theme: &ColorTheme, list: &mut ListState) -> Option<ViewEdit> {
    let mut pick = None;
    let mut edit = None;
    for builtin in BuiltinView::ALL {
        let on = list.tab == ListTab::Builtin(builtin);
        if ui.selectable_label(on, builtin.name()).clicked() {
            pick = Some(ListTab::Builtin(builtin));
        }
    }
    let mut rename = None;
    let mut delete = None;
    for view in &list.saved {
        let on = matches!(&list.tab, ListTab::Saved(slug) if *slug == view.slug);
        let tab = ui.selectable_label(on, &view.spec.name).on_hover_ui(|ui| {
            ui.label(&view.spec.query);
        });
        if tab.clicked() {
            pick = Some(ListTab::Saved(view.slug.clone()));
        }
        tab.context_menu(|ui| {
            if ui.button("Rename…").clicked() {
                rename = Some(view.slug.clone());
                ui.close();
            }
            if ui.button("Delete view").clicked() {
                delete = Some(view.slug.clone());
                ui.close();
            }
        });
    }
    if let Some(tab) = pick {
        list.select_tab(tab);
    }
    if let Some(slug) = rename {
        let name = list.saved_view(&slug).map(|v| v.spec.name.clone());
        list.naming = Some(ViewNaming {
            rename: Some(slug),
            name: name.unwrap_or_default(),
            focus: true,
        });
    }
    if let Some(slug) = delete {
        edit = Some(list.delete_view(&slug));
    }
    if list.naming.is_some() {
        return view_naming_ui(ui, list).or(edit);
    }
    if list.dirty() {
        let save = egui::Button::new(egui::RichText::new("Save").strong().color(theme.accent));
        if ui
            .add(save)
            .on_hover_text("Save these changes to the view")
            .clicked()
        {
            edit = list.save_over_tab();
        }
    }
    let new = egui::Button::new(egui::RichText::new("+ View").color(theme.text_muted)).frame(false);
    if ui
        .add(new)
        .on_hover_text("Save what the list shows as a new view")
        .clicked()
    {
        list.naming = Some(ViewNaming {
            rename: None,
            name: String::new(),
            focus: true,
        });
    }
    edit
}

/// The view-name field: Enter names the new view (or renames the one it's
/// for), Esc or an empty name closes it. Returns the edit.
fn view_naming_ui(ui: &mut egui::Ui, list: &mut ListState) -> Option<ViewEdit> {
    let naming = list.naming.as_mut()?;
    let field = egui::TextEdit::singleline(&mut naming.name)
        .hint_text("View name…")
        .desired_width(160.0);
    let response = ui.add(field);
    if std::mem::take(&mut naming.focus) {
        response.request_focus();
    }
    let (enter, escape) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::Enter),
            i.key_pressed(egui::Key::Escape),
        )
    });
    let submit = (enter && response.lost_focus()) || ui.button("Save view").clicked();
    if escape {
        list.naming = None;
        return None;
    }
    if !submit {
        return None;
    }
    let naming = list.naming.take()?;
    let name = naming.name.trim();
    if name.is_empty() {
        return None;
    }
    match naming.rename {
        Some(slug) => list.rename_view(&slug, name),
        None => Some(list.save_new_view(name)),
    }
}

/// The list's Display menu, like Linear's view options: the grouping and
/// sub-grouping, whether empty groups show, the ordering, its direction and
/// whether done rows trail by recency, one toggle chip per [`DisplayProp`],
/// and whether sub-issues are listed. Stays open while chips are flipped. The trigger wears the
/// filter's warm tint while any of it differs from the default, so a list
/// laid out other than usual says so. Returns whether the menu is open.
fn display_menu_ui(ui: &mut egui::Ui, theme: &ColorTheme, menu: DisplayMenu) -> bool {
    let DisplayMenu {
        display,
        grouping,
        sorting,
        hide_subissues,
    } = menu;
    let defaults = ListState::default();
    let label = if *display == DisplayProps::default()
        && *grouping == defaults.grouping
        && *sorting == defaults.sorting
        && !*hide_subissues
    {
        egui::RichText::new("☰ Display").color(theme.text_secondary)
    } else {
        egui::RichText::new("☰ Display")
            .strong()
            .color(theme.warning)
    };
    // Closes on a click outside only, so several chips can be flipped in
    // one visit.
    let config = egui::containers::menu::MenuConfig::new()
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    let menu = egui::containers::menu::MenuButton::new(label).config(config);
    let (_, open) = menu.ui(ui, |ui| {
        ui.set_max_width(280.0);
        let heading = |ui: &mut egui::Ui, text: &str| {
            ui.label(egui::RichText::new(text).small().color(theme.text_muted));
            ui.add_space(SPACING_XS);
        };
        heading(ui, "Grouping");
        group_chips_ui(ui, &mut grouping.by, "headway-list-grouping");
        ui.add_space(SPACING_SM);
        heading(ui, "Sub-grouping");
        group_chips_ui(ui, &mut grouping.sub, "headway-list-sub-grouping");
        ui.add_space(SPACING_XS);
        ui.checkbox(&mut grouping.show_empty, "Show empty groups");
        ui.add_space(SPACING_SM);
        heading(ui, "Ordering");
        sort_chips_ui(ui, sorting);
        ui.add_space(SPACING_XS);
        ui.checkbox(
            &mut sorting.completed_by_recency,
            "Order completed by recency",
        );
        ui.add_space(SPACING_SM);
        ui.label(
            egui::RichText::new("Display properties")
                .small()
                .color(theme.text_muted),
        );
        ui.add_space(SPACING_XS);
        ui.horizontal_wrapped(|ui| {
            for prop in DisplayProp::ALL {
                let on = display.contains(prop);
                if ui.selectable_label(on, prop.label()).clicked() {
                    display.toggle(prop);
                }
            }
        });
        ui.add_space(SPACING_XS);
        let mut subissues = !*hide_subissues;
        if ui.checkbox(&mut subissues, "Show sub-issues").changed() {
            *hide_subissues = !subissues;
        }
        ui.add_space(SPACING_XS);
        if ui.button("Reset to default").clicked() {
            *display = DisplayProps::default();
            *grouping = defaults.grouping;
            *sorting = defaults.sorting;
            *hide_subissues = false;
        }
    });
    open.is_some()
}

/// What the Display menu edits: the list's layout.
struct DisplayMenu<'a> {
    display: &'a mut DisplayProps,
    grouping: &'a mut Grouping,
    sorting: &'a mut Sorting,
    hide_subissues: &'a mut bool,
}

/// One chip per [`GroupBy`], the one `by` holds selected; a click picks it.
/// `id` keeps the grouping's chips apart from the sub-grouping's, which carry
/// the same names.
fn group_chips_ui(ui: &mut egui::Ui, by: &mut GroupBy, id: &str) {
    ui.push_id(id, |ui| {
        ui.horizontal_wrapped(|ui| {
            for choice in GroupBy::ALL {
                if ui.selectable_label(*by == choice, choice.label()).clicked() {
                    *by = choice;
                }
            }
        });
    });
}

/// One chip per [`SortBy`], the one `sorting` holds selected, then a
/// direction toggle. Picking an ordering runs it its natural way
/// ([`Sorting::by`]); the toggle flips that.
fn sort_chips_ui(ui: &mut egui::Ui, sorting: &mut Sorting) {
    ui.push_id("headway-list-ordering", |ui| {
        ui.horizontal_wrapped(|ui| {
            for choice in SortBy::ALL {
                if ui
                    .selectable_label(sorting.by == choice, choice.label())
                    .clicked()
                    && sorting.by != choice
                {
                    *sorting = Sorting {
                        completed_by_recency: sorting.completed_by_recency,
                        ..Sorting::by(choice)
                    };
                }
            }
            let (arrow, hint) = if sorting.descending {
                ("↓", "Descending")
            } else {
                ("↑", "Ascending")
            };
            if ui.button(arrow).on_hover_text(hint).clicked() {
                sorting.descending = !sorting.descending;
            }
        });
    });
}

/// The list's headers and rows, only those on screen laid out: every entry
/// is [`ROW_HEIGHT`] tall, a header as much as a row. Returns what was
/// clicked this frame. Reads `list` only; the caller acts on the response.
fn list_rows_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    sources: &[BoardRef],
    list: &ListState,
) -> RowsResponse {
    let mut response = RowsResponse::default();
    if list.entries.is_empty() {
        ui.label(egui::RichText::new("No matching issues.").color(theme.text_muted));
        return response;
    }
    let cursor = list.cursor_entry();
    let scroll = list.scroll_to_cursor;
    let display = list.grouping.row_display(list.display);
    let row_height = ROW_HEIGHT;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, row_height, list.entries.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            // The cursor's row, laid out or not: show_rows starts this ui at
            // the first visible row, so any row's rect is an offset from it.
            if scroll && let Some(at) = cursor {
                let top = ui.max_rect().top() + (at as f32 - range.start as f32) * row_height;
                let rect = egui::Rect::from_min_size(
                    egui::pos2(ui.max_rect().left(), top),
                    egui::vec2(ui.available_width(), row_height),
                );
                ui.scroll_to_rect(rect, None);
            }
            for at in range {
                match &list.entries[at] {
                    ListEntry::Header(header) => {
                        let add = group_add(&header.key, view).is_some();
                        let clicked = group_header_ui(ui, theme, header, add);
                        if clicked.toggle {
                            response.toggle = Some(header.key.clone());
                        }
                        if clicked.add {
                            response.add = Some(header.key.clone());
                        }
                    }
                    ListEntry::Row(row) => {
                        let line = &list.rows[*row];
                        let row = RowLook {
                            display,
                            on_cursor: cursor == Some(at),
                            draggable: list.can_drag(sources, line, view),
                        };
                        let drawn = list_row_ui(ui, theme, sources, line, row);
                        if drawn.response.clicked() {
                            response.clicked = Some(at);
                        }
                        if drawn.dropped.is_some() {
                            response.dropped = drawn.dropped;
                        }
                    }
                }
            }
        });
    response
}

/// What a header was clicked for this frame.
struct HeaderClicks {
    /// The header itself: fold or unfold it.
    toggle: bool,
    /// Its `+`: add a card to its group.
    add: bool,
}

/// A group's header, one row tall: a fold arrow, the group's name and its
/// count, and on the right a `+` when `add` (a card added here can land in
/// the group). A sub-group's header sits indented under its group's.
fn group_header_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    header: &GroupHeader,
    add: bool,
) -> HeaderClicks {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_HEIGHT),
        egui::Sense::hover(),
    );
    // Sensed before its contents, so the `+` drawn over it takes its own
    // clicks and the rest of the header folds.
    let id = egui::Id::unique(("headway-list-group", &header.key));
    let fold = ui.interact(rect, id, egui::Sense::click());
    fold.widget_info(|| {
        let verb = if header.collapsed {
            "Expand"
        } else {
            "Collapse"
        };
        egui::WidgetInfo::labeled(
            egui::accesskit::Role::Button,
            true,
            format!("{verb} {}", header.label),
        )
    });
    let sub = header.key.sub.is_some();
    if !sub {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(RADIUS_MD as u8),
            theme.surface_elevated,
        );
    }
    let indent = if sub {
        SPACING_LG + SPACING_SM
    } else {
        SPACING_SM
    };
    let inner = egui::Rect::from_min_max(
        rect.min + egui::vec2(indent, 0.0),
        rect.max - egui::vec2(SPACING_SM, 0.0),
    );
    let mut clicks = HeaderClicks {
        toggle: fold.clicked(),
        add: false,
    };
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = SPACING_SM;
            let arrow = if header.collapsed { "▸" } else { "▾" };
            let quiet = |text: &str| {
                egui::Label::new(egui::RichText::new(text).color(theme.text_muted))
                    .selectable(false)
            };
            ui.add(quiet(arrow));
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&header.label)
                        .strong()
                        .color(theme.text_primary),
                )
                .selectable(false),
            );
            ui.add(quiet(&header.count));
            if !add {
                return;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let plus = egui::Button::new(egui::RichText::new("+").color(theme.text_muted))
                    .frame(false);
                if ui
                    .add(plus)
                    .on_hover_text("New issue in this group")
                    .clicked()
                {
                    clicks.add = true;
                }
            });
        },
    );
    clicks
}

/// How a row draws this frame, beyond its card.
#[derive(Clone, Copy)]
struct RowLook {
    /// The properties it shows.
    display: DisplayProps,
    /// It's the keyboard cursor's row: ringed.
    on_cursor: bool,
    /// It drags to a new place in work order ([`ListState::can_drag`]).
    draggable: bool,
}

/// What a row did this frame: its response, and a work-order drag let go
/// over it.
struct RowDrawn {
    response: egui::Response,
    dropped: Option<RowDrop>,
}

/// One row, Linear's anatomy left to right: priority, word-id, status
/// circle, the title (cut short to fit), then right-aligned the parent
/// breadcrumb and subissue rollup, labels, estimate, blockers, due date, when
/// it was created and changed, and its board — each drawn only while
/// `look.display` holds it. A cursor row is ringed, a ready row wears an
/// accent bar on its left edge, and while a work-order drag hovers it a line
/// marks the half it would land on.
fn list_row_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    sources: &[BoardRef],
    line: &ListLine,
    look: RowLook,
) -> RowDrawn {
    let RowLook {
        display, on_cursor, ..
    } = look;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_HEIGHT),
        egui::Sense::hover(),
    );
    let board: &BoardView = &sources[line.board];
    let card = line.slot.card(board);

    let corner = egui::CornerRadius::same(RADIUS_MD as u8);
    if on_cursor {
        ui.painter()
            .rect_filled(rect, corner, theme.accent.gamma_multiply(0.15));
    } else if ui.rect_contains_pointer(rect) {
        ui.painter()
            .rect_filled(rect, corner, theme.surface_elevated);
    }

    let inner = rect.shrink2(egui::vec2(SPACING_SM, 0.0));
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = SPACING_SM;
            row_lead_ui(ui, theme, board, card, line, display);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = SPACING_SM;
                row_trail_ui(ui, theme, board, card, line, display);
                ui.add_space(SPACING_XS);
                // The title takes what's left, from the left, cut short with
                // an ellipsis rather than pushing the properties off the row.
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&card.title).color(theme.text_primary),
                        )
                        .selectable(false)
                        .truncate(),
                    );
                });
            });
        },
    );

    // Sensed after its contents, so a click anywhere on the row is the
    // row's rather than a label's.
    let id = egui::Id::unique(("headway-list-row", line.id));
    let sense = if look.draggable {
        egui::Sense::click_and_drag()
    } else {
        egui::Sense::click()
    };
    let response = ui.interact(rect, id, sense);
    if on_cursor {
        ui.painter().rect_stroke(
            rect,
            corner,
            egui::Stroke::new(1.0, theme.accent),
            egui::StrokeKind::Inside,
        );
    }
    if line.ready {
        let bar = egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height()));
        ui.painter()
            .rect_filled(bar, egui::CornerRadius::same(1), theme.accent);
    }
    if look.draggable {
        response.dnd_set_drag_payload(DragRow(line.id));
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }
    let dropped = row_drop_ui(ui, theme, &response, line.id);
    RowDrawn { response, dropped }
}

/// While a work-order drag hovers `row` (card `onto`), mark the edge it would
/// land at; when it's let go there, the drop.
fn row_drop_ui(
    ui: &egui::Ui,
    theme: &ColorTheme,
    row: &egui::Response,
    onto: NoteId,
) -> Option<RowDrop> {
    let pointer = ui.input(|i| i.pointer.interact_pos())?;
    let below = pointer.y >= row.rect.center().y;
    if row.dnd_hover_payload::<DragRow>().is_some() {
        let y = if below {
            row.rect.bottom()
        } else {
            row.rect.top()
        };
        ui.painter()
            .hline(row.rect.x_range(), y, egui::Stroke::new(2.0, theme.accent));
    }
    let card = row.dnd_release_payload::<DragRow>()?.0;
    Some(RowDrop { card, onto, below })
}

/// A row's leading properties, before its title: priority, word-id and
/// status circle. Priority keeps its slot when the card has none, so the refs
/// line up down the list.
fn row_lead_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    board: &BoardView,
    card: &CardView,
    line: &ListLine,
    display: DisplayProps,
) {
    if display.contains(DisplayProp::Priority) {
        if card.priority == Priority::None {
            ui.add_space(ICON_SIZE);
        } else {
            priority_icon_ui(ui, theme, card.priority, ICON_SIZE)
                .on_hover_text(priority_label(card.priority));
        }
    }
    if display.contains(DisplayProp::Id) {
        // A fixed-width cell, wide enough for most word-ids, so the circles
        // after them line up down the list.
        let id = egui::Label::new(
            egui::RichText::new(&line.word_id)
                .small()
                .color(theme.text_muted.gamma_multiply(0.7)),
        )
        .selectable(false)
        .truncate();
        let (cell, _) =
            ui.allocate_exact_size(egui::vec2(ID_WIDTH, ROW_HEIGHT), egui::Sense::hover());
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(cell)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| ui.add(id),
        );
    }
    if display.contains(DisplayProp::Status) {
        status_icon_ui(ui, theme, slot_icon(line.slot, board), ICON_SIZE)
            .on_hover_text(slot_place(line.slot, board));
    }
}

/// A row's trailing properties, right-aligned, drawn right to left (so in
/// reverse of how they read): board, updated, created, due, blockers,
/// estimate, labels, then the subissue rollup and parent breadcrumb nearest
/// the title.
fn row_trail_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    board: &BoardView,
    card: &CardView,
    line: &ListLine,
    display: DisplayProps,
) {
    let text = &line.text;
    let muted = |ui: &mut egui::Ui, text: &str| {
        ui.add(
            egui::Label::new(egui::RichText::new(text).small().color(theme.text_muted))
                .selectable(false),
        )
    };
    if display.contains(DisplayProp::Board) {
        muted(ui, &board.title);
    }
    if display.contains(DisplayProp::Updated) {
        muted(ui, &text.updated).on_hover_text("Updated");
    }
    if display.contains(DisplayProp::Created) {
        muted(ui, &text.created).on_hover_text("Created");
    }
    if display.contains(DisplayProp::Due)
        && let Some(due) = &text.due
    {
        muted(ui, due).on_hover_text("Due date");
    }
    if display.contains(DisplayProp::Blockers)
        && let Some(blockers) = &text.blockers
    {
        // Red while any blocker is unfinished; muted once they're all done.
        let color = if card.is_blocked() {
            theme.destructive
        } else {
            theme.text_muted
        };
        tinted_pill(ui, blockers, color).on_hover_text("Blocked by");
    }
    if display.contains(DisplayProp::Estimate)
        && let Some(estimate) = &text.estimate
    {
        text_pill(ui, theme, estimate).on_hover_text("Estimate");
    }
    if display.contains(DisplayProp::Labels) {
        for label in card.labels.iter().rev() {
            label_chip(ui, theme, label);
        }
    }
    if !display.contains(DisplayProp::SubIssues) {
        return;
    }
    if let Some(subissues) = &text.subissues {
        text_pill(ui, theme, subissues).on_hover_text("Sub-issues done");
    }
    if let Some(parent) = &text.parent {
        muted(ui, parent).on_hover_text("Parent");
    }
}

/// Side of a row's priority and status icons.
const ICON_SIZE: f32 = 14.0;

/// Width a row gives its word-id.
const ID_WIDTH: f32 = 150.0;
