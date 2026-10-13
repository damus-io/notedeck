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
use notedeck::tokens::{
    RADIUS_MD, RADIUS_PILL, SPACING_LG, SPACING_MD, SPACING_SM, SPACING_XS, STROKE_THIN,
};

use super::widgets::{
    KeyedText, StatusIcon, label_color, pill_height, priority_icon_ui, priority_label,
    status_icon_ui, text_pill, tinted_pill,
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
    /// The card or group header under the keyboard cursor. A card or a group
    /// rather than an entry index, so a re-fold that reorders the rows, or a
    /// header folding, leaves it on the same one.
    cursor: Option<ListCursor>,
    /// Whether `c` has a group to compose a card in: the cursor sits in a
    /// group that takes a card on the board showing. Latched as the list
    /// draws ([`ListState::latch_compose`]), since chrome asks before then.
    can_compose: bool,
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
            can_compose: false,
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
pub(crate) struct CollapseKey {
    /// The top-level group.
    group: GroupKey,
    /// The sub-group within it, for a sub-group's header.
    sub: Option<GroupKey>,
}

/// Where the issue list's keyboard cursor rests: on a card's row, or on a
/// group's header (headway:notedeck/ice-basket-taxi).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ListCursor {
    /// The row of this card: the first, for a card listed under two labels.
    Card(NoteId),
    /// The header of this group or sub-group.
    Group(CollapseKey),
}

/// What a fold key does to the list's group headers (`za`, `zc`, `zo`, `zM`,
/// `zR`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ListFold {
    /// Fold the cursor's group shut, or open it again (`za`).
    Toggle,
    /// Open the cursor's group (`zo`).
    Open,
    /// Fold the cursor's group shut (`zc`).
    Close,
    /// Fold every group and sub-group shut (`zM`).
    CloseAll,
    /// Open every group (`zR`).
    OpenAll,
}

/// Which way `]g`/`[g` step through the list's group headers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupStep {
    /// To the next header down (`]g`).
    Next,
    /// To the previous header up (`[g`).
    Prev,
}

/// A change to the issue list's layout, as its view tabs and Display menu
/// make one by mouse: the `headway_list_*` UI tools carry these, so keys and
/// the action bar reach them too (headway:notedeck/mercy-unique-foil).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ListLayout {
    /// Show a view tab (`gt`/`gT` step through them).
    Tab(TabPick),
    /// Group the rows.
    Group(Pick<GroupBy>),
    /// Split each group again.
    SubGroup(Pick<GroupBy>),
    /// Order the rows, run the ordering's natural way (`go` cycles them).
    Order(Pick<SortBy>),
    /// Run the ordering the other way, or one way.
    OrderDir(OrderDir),
    /// Show or hide one of a row's properties.
    Display(DisplaySwitch),
    /// Open the view-name field to save what the list shows as a new view
    /// (`gs`), as "+ View" does.
    SaveView,
}

/// Which view tab [`ListLayout::Tab`] shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TabPick {
    /// The tab after the one showing, wrapping: built-ins, then saved views.
    Next,
    /// The tab before the one showing, wrapping.
    Prev,
    /// A built-in view.
    Builtin(BuiltinView),
    /// The saved view with this slug, or else this name (any case).
    Saved(String),
}

/// A pick from one of the Display menu's rows of chips: one chip, or the one
/// after the chip picked now, wrapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pick<T> {
    /// The next chip in the menu's order.
    Next,
    /// This chip.
    To(T),
}

impl<T: Copy + PartialEq> Pick<T> {
    /// What the pick picks when `now` is picked, out of `all` in the menu's
    /// order.
    fn from(self, now: T, all: &[T]) -> T {
        match self {
            Pick::To(to) => to,
            Pick::Next => {
                let at = all.iter().position(|&x| x == now).map_or(0, |at| at + 1);
                all[at % all.len()]
            }
        }
    }
}

/// Which way [`ListLayout::OrderDir`] runs the ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OrderDir {
    /// The other way from now, as the menu's arrow does.
    Toggle,
    /// Low to high.
    Ascending,
    /// High to low.
    Descending,
}

/// What [`ListLayout::Display`] does to a row property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DisplaySwitch {
    /// The property.
    pub(crate) prop: DisplayProp,
    /// Show it (`Some(true)`), hide it, or flip it (`None`), as its chip does.
    pub(crate) on: Option<bool>,
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
    /// The glyph before its name, Linear's way: a status circle, a priority
    /// icon or a label's colour.
    icon: HeaderIcon,
}

/// The glyph a [`GroupHeader`] leads with, by what it groups.
#[derive(Clone, Copy)]
enum HeaderIcon {
    /// A status group: the circle its cards wear.
    Status(StatusIcon),
    /// A priority group: that priority's icon.
    Priority(Priority),
    /// A label group: a dot of the label's colour.
    Label(egui::Color32),
    /// A board or parent group: nothing.
    None,
}

impl HeaderIcon {
    /// The glyph for group `g`, whose rows index `lines`. A status group
    /// takes its first row's circle (columns are matched by name across
    /// boards, so the group has no single board to ask); an empty one, the
    /// plain ring.
    fn of(g: &ListGroup, lines: &[ListLine]) -> HeaderIcon {
        match &g.key {
            GroupKey::Status(_) => HeaderIcon::Status(
                g.rows
                    .first()
                    .and_then(|&at| lines.get(at))
                    .map_or(StatusIcon::Todo, |line| line.icon),
            ),
            GroupKey::Archived => HeaderIcon::Status(StatusIcon::Archived),
            GroupKey::Priority(p) => HeaderIcon::Priority(*p),
            GroupKey::Label(Some(label)) => HeaderIcon::Label(label_color(label)),
            _ => HeaderIcon::None,
        }
    }
}

impl GroupHeader {
    /// The header for group `g`, under `key`; its rows index `lines`.
    fn of(key: CollapseKey, g: &ListGroup, collapsed: bool, lines: &[ListLine]) -> GroupHeader {
        GroupHeader {
            key,
            label: g.label.clone(),
            count: g.rows.len().to_string(),
            collapsed,
            icon: HeaderIcon::of(g, lines),
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
    /// Its status circle, decided once with its board in hand, so a group
    /// header (which has no board) can wear its first row's.
    icon: StatusIcon,
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

    /// The card under the keyboard cursor, if it's on a card's row.
    pub(crate) fn cursor(&self) -> Option<NoteId> {
        match self.cursor {
            Some(ListCursor::Card(card)) => Some(card),
            _ => None,
        }
    }

    /// Where the keyboard cursor rests, card or header.
    pub(crate) fn cursor_spot(&self) -> Option<&ListCursor> {
        self.cursor.as_ref()
    }

    /// Whether the list draws group headers, for the fold and `]g`/`[g` keys
    /// to act on. Allocates nothing: chrome asks every frame.
    pub(crate) fn has_groups(&self) -> bool {
        matches!(self.entries.first(), Some(ListEntry::Header(_)))
    }

    /// Whether `c` has a group to compose a card in (see
    /// [`can_compose`](Self::can_compose)'s field).
    pub(crate) fn can_compose(&self) -> bool {
        self.can_compose
    }

    /// How many headers and rows the list draws: fewer once a group folds.
    #[cfg(test)]
    pub(crate) fn shown(&self) -> usize {
        self.entries.len()
    }

    /// Put the cursor at `to`, for a test.
    #[cfg(test)]
    pub(crate) fn set_cursor(&mut self, to: ListCursor) {
        self.cursor = Some(to);
    }

    /// The header key of status group `name`, for a test.
    #[cfg(test)]
    pub(crate) fn status_group(name: &str) -> CollapseKey {
        CollapseKey {
            group: GroupKey::Status(name.to_string()),
            sub: None,
        }
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
                icon: slot_icon(pos.slot, row.board),
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
            self.entries.push(ListEntry::Header(GroupHeader::of(
                key, g, collapsed, &self.rows,
            )));
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
                self.entries.push(ListEntry::Header(GroupHeader::of(
                    key, sub, collapsed, &self.rows,
                )));
                if !collapsed {
                    self.entries
                        .extend(sub.rows.iter().copied().map(ListEntry::Row));
                }
            }
        }
    }

    /// Fold the header `key` shut, or open it again, as a click on it does.
    fn toggle_collapsed(&mut self, key: &CollapseKey) {
        let here = self.cursor_group().cloned();
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.clone());
        }
        self.rebuild_entries();
        self.settle_cursor(here);
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
        self.can_compose = false;
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

    /// Change the layout as `change` says, as the tab or Display-menu chip
    /// it names would. The rows follow on the next
    /// [`refresh`](Self::refresh).
    pub(crate) fn apply_layout(&mut self, change: &ListLayout) {
        match change {
            ListLayout::Tab(pick) => self.pick_tab(pick),
            ListLayout::Group(pick) => {
                self.grouping.by = pick.from(self.grouping.by, &GroupBy::ALL);
            }
            ListLayout::SubGroup(pick) => {
                self.grouping.sub = pick.from(self.grouping.sub, &GroupBy::ALL);
            }
            ListLayout::Order(pick) => {
                let by = pick.from(self.sorting.by, &SortBy::ALL);
                reorder(&mut self.sorting, by);
            }
            ListLayout::OrderDir(dir) => {
                self.sorting.descending = match dir {
                    OrderDir::Toggle => !self.sorting.descending,
                    OrderDir::Ascending => false,
                    OrderDir::Descending => true,
                };
            }
            ListLayout::Display(DisplaySwitch { prop, on: None }) => self.display.toggle(*prop),
            ListLayout::Display(DisplaySwitch { prop, on: Some(on) }) => {
                self.display.set(*prop, *on)
            }
            ListLayout::SaveView => self.start_naming(None),
        }
    }

    /// Open the view-name field: naming a new view of what the list shows,
    /// or renaming saved view `rename`.
    fn start_naming(&mut self, rename: Option<String>) {
        let name = rename
            .as_deref()
            .and_then(|slug| self.saved_view(slug))
            .map(|v| v.spec.name.clone());
        self.naming = Some(ViewNaming {
            rename,
            name: name.unwrap_or_default(),
            focus: true,
        });
    }

    /// Show the tab `pick` picks. A saved view the account hasn't is
    /// logged and ignored.
    fn pick_tab(&mut self, pick: &TabPick) {
        let count = BuiltinView::ALL.len() + self.saved.len();
        let tab = match pick {
            TabPick::Builtin(builtin) => ListTab::Builtin(*builtin),
            TabPick::Saved(name) => {
                let found = self.saved_view(name).or_else(|| {
                    self.saved
                        .iter()
                        .find(|v| v.spec.name.eq_ignore_ascii_case(name.trim()))
                });
                let Some(view) = found else {
                    tracing::debug!("headway: no saved view {name:?}; tab unchanged");
                    return;
                };
                ListTab::Saved(view.slug.clone())
            }
            TabPick::Next => self.tab_at((self.tab_index() + 1) % count),
            TabPick::Prev => self.tab_at((self.tab_index() + count - 1) % count),
        };
        self.select_tab(tab);
    }

    /// Where the showing tab sits in the tab row: the built-ins, then the
    /// saved views.
    fn tab_index(&self) -> usize {
        match &self.tab {
            ListTab::Builtin(builtin) => BuiltinView::ALL
                .iter()
                .position(|b| b == builtin)
                .unwrap_or(0),
            ListTab::Saved(slug) => {
                let at = self.saved.iter().position(|v| v.slug == *slug);
                BuiltinView::ALL.len() + at.unwrap_or(0)
            }
        }
    }

    /// The tab at `at` in the tab row; `at` is below the tab count.
    fn tab_at(&self, at: usize) -> ListTab {
        match BuiltinView::ALL.get(at) {
            Some(builtin) => ListTab::Builtin(*builtin),
            None => ListTab::Saved(self.saved[at - BuiltinView::ALL.len()].slug.clone()),
        }
    }

    /// What the list shows, as a view: its tab's name, query and layout,
    /// for a test to read.
    #[cfg(test)]
    pub(crate) fn layout(&self) -> ViewSpec {
        self.spec()
    }

    /// Whether the view-name field is open, for a test to read.
    #[cfg(test)]
    pub(crate) fn naming(&self) -> bool {
        self.naming.is_some()
    }

    /// The row (an index into [`rows`](Self::rows)) entry `at` draws, if
    /// it's a row rather than a header.
    fn entry_row(&self, at: usize) -> Option<usize> {
        match self.entries.get(at)? {
            ListEntry::Row(row) => Some(*row),
            ListEntry::Header(_) => None,
        }
    }

    /// The header entry `at` draws, if it's a header rather than a row.
    fn entry_header(&self, at: usize) -> Option<&GroupHeader> {
        match self.entries.get(at)? {
            ListEntry::Header(header) => Some(header),
            ListEntry::Row(_) => None,
        }
    }

    /// Whether entry `at` is where `cursor` rests: its card's row, or its
    /// group's header.
    fn entry_is(&self, at: usize, cursor: &ListCursor) -> bool {
        match (&self.entries[at], cursor) {
            (ListEntry::Row(row), ListCursor::Card(card)) => self.rows[*row].id == *card,
            (ListEntry::Header(header), ListCursor::Group(key)) => header.key == *key,
            _ => false,
        }
    }

    /// The cursor for entry `at`: its card, or its header's group.
    fn cursor_of(&self, at: usize) -> Option<ListCursor> {
        Some(match self.entries.get(at)? {
            ListEntry::Row(row) => ListCursor::Card(self.rows[*row].id),
            ListEntry::Header(header) => ListCursor::Group(header.key.clone()),
        })
    }

    /// The entry the cursor rests on, if the list shows it: the first, for a
    /// card listed under two labels.
    fn cursor_entry(&self) -> Option<usize> {
        let cursor = self.cursor.as_ref()?;
        (0..self.entries.len()).find(|&at| self.entry_is(at, cursor))
    }

    /// The group the cursor is in: the header it rests on, or the nearest
    /// header above its row (a sub-group's, when the list is sub-grouped).
    /// `None` without a cursor the list shows, or in an ungrouped list.
    /// Allocates nothing: the list asks every frame
    /// ([`latch_compose`](Self::latch_compose)).
    fn cursor_group(&self) -> Option<&CollapseKey> {
        let at = self.cursor_entry()?;
        (0..=at)
            .rev()
            .find_map(|at| self.entry_header(at))
            .map(|header| &header.key)
    }

    /// Put the cursor on entry `at` and scroll it into view.
    fn put_cursor(&mut self, at: usize) {
        if let Some(cursor) = self.cursor_of(at) {
            self.cursor = Some(cursor);
            self.scroll_to_cursor = true;
        }
    }

    /// Move the cursor an entry down or up, or to the first or last entry,
    /// group headers included. With no cursor (or one on a card or group the
    /// list no longer shows), any move lands on the first entry. Left and
    /// right mean nothing in one list.
    pub(crate) fn move_cursor(&mut self, mv: CursorMove) {
        let last = self.entries.len().checked_sub(1);
        let to = match (self.cursor_entry(), mv) {
            (None, _) | (_, CursorMove::First) => last.map(|_| 0),
            (_, CursorMove::Last) => last,
            (Some(at), CursorMove::Down) => (at + 1 < self.entries.len()).then_some(at + 1),
            (Some(at), CursorMove::Up) => at.checked_sub(1),
            (Some(_), CursorMove::Left | CursorMove::Right) => return,
        };
        if let Some(at) = to {
            self.put_cursor(at);
        }
    }

    /// Move the cursor to the next or previous group header (`]g`/`[g`).
    /// With no cursor, `]g` lands on the first header and `[g` on the last.
    pub(crate) fn step_group(&mut self, step: GroupStep) {
        let is_header = |at: &usize| self.entry_header(*at).is_some();
        let here = self.cursor_entry();
        let to = match (here, step) {
            (None, GroupStep::Next) => (0..self.entries.len()).find(is_header),
            (None, GroupStep::Prev) => (0..self.entries.len()).rev().find(is_header),
            (Some(at), GroupStep::Next) => (at + 1..self.entries.len()).find(is_header),
            (Some(at), GroupStep::Prev) => (0..at).rev().find(is_header),
        };
        if let Some(at) = to {
            self.put_cursor(at);
        }
    }

    /// Fold the list's groups as `fold` says: the cursor's group, or all of
    /// them. A cursor whose row folds away moves up onto the header that
    /// hid it.
    pub(crate) fn fold(&mut self, fold: ListFold) {
        let here = self.cursor_group().cloned();
        match fold {
            ListFold::Toggle | ListFold::Open | ListFold::Close => {
                let Some(key) = here.clone() else {
                    return;
                };
                let shut = self.collapsed.contains(&key);
                let close = match fold {
                    ListFold::Toggle => !shut,
                    ListFold::Close => true,
                    _ => false,
                };
                if close == shut {
                    return;
                }
                if close {
                    self.collapsed.insert(key);
                } else {
                    self.collapsed.remove(&key);
                }
            }
            ListFold::CloseAll => {
                for g in &self.groups {
                    let group = || CollapseKey {
                        group: g.key.clone(),
                        sub: None,
                    };
                    self.collapsed.insert(group());
                    self.collapsed
                        .extend(g.subgroups.iter().map(|sub| CollapseKey {
                            sub: Some(sub.key.clone()),
                            ..group()
                        }));
                }
            }
            ListFold::OpenAll => self.collapsed.clear(),
        }
        self.rebuild_entries();
        self.settle_cursor(here);
    }

    /// After a fold, move a cursor whose row or sub-group header folded away
    /// onto the header that hid it: `here`, the group it was in before the
    /// fold, while that still shows, else `here`'s top-level group.
    fn settle_cursor(&mut self, here: Option<CollapseKey>) {
        if self.cursor.is_none() || self.cursor_entry().is_some() {
            return;
        }
        let Some(here) = here else {
            return;
        };
        let shows = |key: &CollapseKey| {
            (0..self.entries.len()).any(|at| self.entry_header(at).is_some_and(|h| h.key == *key))
        };
        let to = if shows(&here) {
            here
        } else {
            CollapseKey { sub: None, ..here }
        };
        self.cursor = Some(ListCursor::Group(to));
        self.scroll_to_cursor = true;
    }

    /// Latch whether `c` has a group to compose in: the cursor's group takes
    /// a card on `view`, the board showing ([`group_add`]). Run as the list
    /// draws, for chrome to ask before the next frame's keys. Allocates
    /// nothing.
    fn latch_compose(&mut self, view: &BoardView) {
        self.can_compose = self
            .cursor_group()
            .is_some_and(|key| group_add(key, view).is_some());
    }

    /// Open the composer for a card in the cursor's group, as its header's
    /// `+` does (`c`), if a card added there can land in it on `view`.
    pub(crate) fn compose_at_cursor(&mut self, view: &BoardView) {
        let Some(key) = self
            .cursor_group()
            .filter(|key| group_add(key, view).is_some())
            .cloned()
        else {
            return;
        };
        self.open_compose(key);
    }

    /// Put the cursor back where `to` rests and scroll it into view, if the
    /// list shows that card or header; otherwise leave it where it is.
    pub(crate) fn restore_cursor(&mut self, to: &ListCursor) {
        if let Some(at) = (0..self.entries.len()).find(|&at| self.entry_is(at, to)) {
            self.put_cursor(at);
        }
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

    /// Open the card under the list's cursor, or fold or unfold the group
    /// header it rests on, as a click on either does (`Enter`/`o`).
    pub(crate) fn open_list_cursor(&mut self, view: &BoardView, sources: &[BoardRef]) {
        let Some(at) = self.list.cursor_entry() else {
            return;
        };
        if self.list.entry_header(at).is_some() {
            self.list.fold(ListFold::Toggle);
            return;
        }
        self.open_list_row(view, sources, at);
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
        self.list.cursor = Some(ListCursor::Card(target.card));
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
pub(crate) fn list_view_ui(
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
    state.list.latch_compose(view);
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
        list.start_naming(Some(slug));
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
        list.start_naming(None);
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
            ui.spacing_mut().item_spacing = egui::vec2(SPACING_XS, SPACING_XS);
            ui.spacing_mut().button_padding = egui::vec2(SPACING_SM + 2.0, 3.0);
            for prop in DisplayProp::ALL {
                let on = display.contains(prop);
                if prop_chip_ui(ui, theme, prop.label(), on).clicked() {
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
                {
                    reorder(sorting, choice);
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

/// Order by `by`, run its natural way ([`Sorting::by`]), keeping whether
/// done rows trail by recency. Picking the ordering already picked keeps its
/// direction.
fn reorder(sorting: &mut Sorting, by: SortBy) {
    if sorting.by != by {
        *sorting = Sorting {
            completed_by_recency: sorting.completed_by_recency,
            ..Sorting::by(by)
        };
    }
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
                        let look = HeaderLook {
                            add: group_add(&header.key, view).is_some(),
                            on_cursor: cursor == Some(at),
                        };
                        let clicked = group_header_ui(ui, theme, header, look);
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

/// How a group header draws this frame, beyond its group.
#[derive(Clone, Copy)]
struct HeaderLook {
    /// A card added here can land in the group: it draws a `+`.
    add: bool,
    /// It's the keyboard cursor's header: ringed, as a cursor row is.
    on_cursor: bool,
}

/// A group's header, one row tall: a fold arrow, the group's name and its
/// count, and on the right a `+` when `look.add` (a card added here can land
/// in the group). A sub-group's header sits indented under its group's. The
/// keyboard cursor's header is tinted and ringed, as a cursor row is.
fn group_header_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    header: &GroupHeader,
    look: HeaderLook,
) -> HeaderClicks {
    let HeaderLook { add, on_cursor } = look;
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
    let corner = egui::CornerRadius::same(RADIUS_MD as u8);
    // A quiet band, as Linear's: the panel a shade lighter, never the
    // elevated surface (a slab in dark mode, and white-on-white in light).
    if on_cursor {
        ui.painter()
            .rect_filled(rect, corner, theme.accent.gamma_multiply(0.15));
        ui.painter().rect_stroke(
            rect,
            corner,
            egui::Stroke::new(1.0, theme.accent),
            egui::StrokeKind::Inside,
        );
    } else if !sub {
        ui.painter()
            .rect_filled(rect, corner, theme.surface_secondary);
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
            header_icon_ui(ui, theme, header.icon);
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
                let plus = egui::Button::new(
                    egui::RichText::new("+")
                        .size(ICON_SIZE + 2.0)
                        .color(theme.text_muted),
                )
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
            .rect_filled(rect, corner, theme.surface_secondary);
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
            // An empty icon-sized slot, spaced like the icon it stands in
            // for (`add_space` skips the item spacing, a few points short).
            ui.allocate_exact_size(egui::vec2(ICON_SIZE, ICON_SIZE), egui::Sense::hover());
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
        // Laid out in an ID_WIDTH box that claims all of it: a child ui
        // placed over an already-allocated rect would move the cursor back
        // to the end of its text, and the titles would start ragged.
        ui.allocate_ui_with_layout(
            egui::vec2(ID_WIDTH, ROW_HEIGHT),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(ID_WIDTH);
                ui.add(id)
            },
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
            label_pill_ui(ui, theme, label);
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
const ID_WIDTH: f32 = 128.0;

/// A label as a list row draws it, Linear's way: a thin outline pill with a
/// dot of the label's colour and its name in secondary text. The pill is
/// one fixed height ([`pill_height`]) whatever row it sits in, where the
/// grid's filled [`label_chip`](super::widgets::label_chip) grows to the height it's given.
fn label_pill_ui(ui: &mut egui::Ui, theme: &ColorTheme, label: &str) -> egui::Response {
    const DOT: f32 = 6.0;
    let font = egui::TextStyle::Small.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, theme.text_secondary);
    let size = egui::vec2(
        SPACING_SM + DOT + SPACING_XS + galley.size().x + SPACING_SM,
        pill_height(ui),
    );
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    // Painted, so it names itself to accesskit the way a label would.
    response.widget_info(|| egui::WidgetInfo::labeled(egui::accesskit::Role::Label, true, label));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(RADIUS_PILL as u8),
        egui::Stroke::new(STROKE_THIN, theme.border_strong),
        egui::StrokeKind::Inside,
    );
    let dot = egui::pos2(rect.left() + SPACING_SM + DOT / 2.0, rect.center().y);
    painter.circle_filled(dot, DOT / 2.0, label_color(label));
    let text = egui::pos2(
        rect.left() + SPACING_SM + DOT + SPACING_XS,
        rect.center().y - galley.size().y / 2.0,
    );
    painter.galley(text, galley, theme.text_secondary);
    response
}

/// A group header's leading glyph, [`ICON_SIZE`] square; nothing for
/// [`HeaderIcon::None`].
fn header_icon_ui(ui: &mut egui::Ui, theme: &ColorTheme, icon: HeaderIcon) {
    match icon {
        HeaderIcon::Status(status) => {
            status_icon_ui(ui, theme, status, ICON_SIZE);
        }
        HeaderIcon::Priority(priority) => {
            priority_icon_ui(ui, theme, priority, ICON_SIZE);
        }
        HeaderIcon::Label(color) => {
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(ICON_SIZE, ICON_SIZE), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 4.0, color);
        }
        HeaderIcon::None => {}
    }
}

/// One display-property toggle in the Display menu, Linear's way: an
/// outline pill, filled and in primary text while the property shows, bare
/// and muted while it's hidden.
fn prop_chip_ui(ui: &mut egui::Ui, theme: &ColorTheme, label: &str, on: bool) -> egui::Response {
    let (fill, text) = if on {
        (theme.surface_elevated, theme.text_primary)
    } else {
        (egui::Color32::TRANSPARENT, theme.text_muted)
    };
    let chip = egui::Button::new(egui::RichText::new(label).small().color(text))
        .fill(fill)
        .stroke(egui::Stroke::new(STROKE_THIN, theme.border_strong))
        .corner_radius(egui::CornerRadius::same(RADIUS_PILL as u8));
    ui.add(chip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::tests::{grid, id};
    use std::rc::Rc;

    /// [`grid`] as the one source the list reads.
    fn sources(view: BoardView) -> Vec<BoardRef> {
        let boards: Rc<[BoardView]> = Rc::from(vec![view]);
        vec![BoardRef::find(boards, &[0; 32], "headway").expect("the board")]
    }

    /// A list over [`grid`], on All issues (Active would drop `c`, the last
    /// column, as done) and grouped by status: `a` (1, 2, 3), `b` (4) and `c`
    /// (5, 6), each under its header.
    fn grouped() -> ListState {
        let mut list = ListState {
            base: BuiltinView::All.query().to_string(),
            ..ListState::default()
        };
        list.refresh(&sources(grid()));
        list
    }

    /// The status group named `name`'s header key.
    fn group(name: &str) -> CollapseKey {
        CollapseKey {
            group: GroupKey::Status(name.to_string()),
            sub: None,
        }
    }

    fn on_card(n: u8) -> Option<ListCursor> {
        Some(ListCursor::Card(id(n)))
    }

    fn on_group(name: &str) -> Option<ListCursor> {
        Some(ListCursor::Group(group(name)))
    }

    /// `j` walks the headers along with the rows, top to bottom, and `k`
    /// back up; `gg` and `G` land on the first and last entries, the first a
    /// header.
    #[test]
    fn j_and_k_walk_headers_and_rows() {
        let mut list = grouped();
        let walk = [
            on_group("a"),
            on_card(1),
            on_card(2),
            on_card(3),
            on_group("b"),
            on_card(4),
            on_group("c"),
            on_card(5),
            on_card(6),
        ];
        for want in &walk {
            list.move_cursor(CursorMove::Down);
            assert_eq!(&list.cursor, want);
        }
        // Clamped at the bottom.
        list.move_cursor(CursorMove::Down);
        assert_eq!(list.cursor, on_card(6));
        for want in walk.iter().rev().skip(1) {
            list.move_cursor(CursorMove::Up);
            assert_eq!(&list.cursor, want);
        }
        list.move_cursor(CursorMove::Last);
        assert_eq!(list.cursor, on_card(6));
        list.move_cursor(CursorMove::First);
        assert_eq!(list.cursor, on_group("a"));
        // On a header there's no card for the card keys to act on.
        assert_eq!(list.cursor(), None);
    }

    /// `]g` and `[g` step from header to header, from a row too; with no
    /// cursor they land on the first and the last header.
    #[test]
    fn step_group_goes_header_to_header() {
        let mut list = grouped();
        list.step_group(GroupStep::Prev);
        assert_eq!(list.cursor, on_group("c"));
        list.cursor = None;
        list.step_group(GroupStep::Next);
        assert_eq!(list.cursor, on_group("a"));
        list.step_group(GroupStep::Next);
        assert_eq!(list.cursor, on_group("b"));
        list.cursor = on_card(5);
        list.step_group(GroupStep::Prev);
        assert_eq!(list.cursor, on_group("c"));
        list.step_group(GroupStep::Next);
        assert_eq!(list.cursor, on_group("c"), "no header below c's");
    }

    /// `za` on a row folds its group, moving the cursor up onto the header
    /// that hid it, and `j` then skips the folded rows; `za` again opens it.
    #[test]
    fn toggling_a_rows_group_moves_the_cursor_onto_its_header() {
        let mut list = grouped();
        list.cursor = on_card(2);
        list.fold(ListFold::Toggle);
        assert!(list.collapsed.contains(&group("a")));
        assert_eq!(list.cursor, on_group("a"));
        list.move_cursor(CursorMove::Down);
        assert_eq!(list.cursor, on_group("b"), "a's rows are folded away");
        list.move_cursor(CursorMove::Up);
        list.fold(ListFold::Toggle);
        assert!(list.collapsed.is_empty());
        assert_eq!(list.cursor, on_group("a"), "the cursor stays on the header");
        list.move_cursor(CursorMove::Down);
        assert_eq!(list.cursor, on_card(1));
    }

    /// `zc` only closes and `zo` only opens; neither moves a cursor that
    /// still shows.
    #[test]
    fn close_and_open_only_close_and_open() {
        let mut list = grouped();
        list.cursor = on_card(4);
        list.fold(ListFold::Open);
        assert!(list.collapsed.is_empty());
        assert_eq!(list.cursor, on_card(4));
        list.fold(ListFold::Close);
        list.fold(ListFold::Close);
        assert!(list.collapsed.contains(&group("b")));
        assert_eq!(list.cursor, on_group("b"));
        list.fold(ListFold::Open);
        list.fold(ListFold::Open);
        assert!(list.collapsed.is_empty());
    }

    /// `zM` folds every group and sub-group, leaving the cursor on its row's
    /// top-level header; `zR` opens them all.
    #[test]
    fn fold_all_and_open_all() {
        let mut list = grouped();
        list.grouping.sub = GroupBy::Priority;
        list.refresh(&sources(grid()));
        list.cursor = on_card(5);
        list.fold(ListFold::CloseAll);
        // Three groups, each with its one "No priority" sub-group.
        assert_eq!(list.collapsed.len(), 6);
        assert_eq!(list.entries.len(), 3, "only the top-level headers show");
        assert_eq!(list.cursor, on_group("c"));
        list.fold(ListFold::OpenAll);
        assert!(list.collapsed.is_empty());
        assert_eq!(list.entries.len(), 3 + 3 + 6);
        assert_eq!(list.cursor, on_group("c"));
    }

    /// A board re-fold that moves the cursor's header leaves the cursor on
    /// the same group.
    #[test]
    fn a_refold_keeps_the_cursor_on_its_group() {
        let mut list = grouped();
        list.cursor = on_group("b");
        let at = list.cursor_entry();
        // 5 moves from `c` up into `a`, above `b`'s header.
        let mut view = grid();
        let five = view.columns[2].cards.remove(0);
        view.columns[0].cards.push(five);
        list.refresh(&sources(view));
        assert_eq!(list.cursor, on_group("b"));
        assert_eq!(list.cursor_entry(), at.map(|at| at + 1));
    }

    /// `c` opens the composer for the cursor's group, from its header or a
    /// row under it, and is offered only where a card can land: the board
    /// showing's own groups.
    #[test]
    fn c_composes_in_the_cursors_group() {
        let view = grid();
        let mut list = grouped();
        list.latch_compose(&view);
        assert!(!list.can_compose(), "no cursor, no group");
        list.cursor = on_card(4);
        list.latch_compose(&view);
        assert!(list.can_compose());
        list.compose_at_cursor(&view);
        let compose = list.compose.as_ref().expect("the composer");
        assert_eq!(compose.key, group("b"));
        assert_eq!(compose.label, "b");

        // A board where `b` isn't a column takes no card in its group.
        let mut other = grid();
        other.columns.remove(1);
        list.compose = None;
        list.latch_compose(&other);
        assert!(!list.can_compose());
        list.compose_at_cursor(&other);
        assert!(list.compose.is_none());
    }

    /// Ungrouped, the list has no headers: the cursor walks rows alone and the
    /// fold keys aren't offered.
    #[test]
    fn an_ungrouped_list_has_no_headers() {
        let mut list = grouped();
        list.grouping.by = GroupBy::None;
        list.refresh(&sources(grid()));
        assert!(!list.has_groups());
        list.move_cursor(CursorMove::First);
        assert_eq!(list.cursor, on_card(1));
        list.fold(ListFold::Toggle);
        assert!(list.collapsed.is_empty());
        assert!(grouped().has_groups());
    }
}
