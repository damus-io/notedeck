//! The issue-list query model: which cards a list view shows, across one board
//! or many.
//!
//! One parser and one predicate shared by every front end — the GUI board
//! filter, the GUI list surface and `headway list` — so a query typed into any
//! of them means the same thing. Pure: it reads folded [`BoardView`]s and
//! allocates nothing it doesn't hand back.
//!
//! - [`IssueFilter`] — a parsed, Linear-ish query (`p:high label:ui is:done`)
//!   and the predicate it stands for.
//! - [`ListRow`] — a card plus the board it's on and its [`Placement`] there
//!   (a column, or the archive), the unit every predicate and renderer reads.
//! - [`ListQuery`] + [`list`] — flatten boards into the rows a query keeps;
//!   [`list_positions`] is the same walk as indices ([`RowPos`]).
//! - [`Grouping`] + [`group`] — gather rows under headers.
//! - [`Sorting`] + [`sort_order`] — order rows, by work order among others,
//!   with [`ready_set`] marking what `headway next --ready` hands out.

use std::collections::{HashMap, HashSet};

use nostrdb_net::NoteId;

use crate::event::{self, BoardView, CardView, ColumnView, Date, Priority};

/// Where a [`ListRow`]'s card sits on its board: in a live column, or taken
/// off the board into its archive ([`BoardView::archived`]).
#[derive(Clone, Copy, Debug)]
pub enum Placement<'a> {
    /// In this live column.
    Column(&'a ColumnView),
    /// Archived: on the board, but in no column.
    Archived,
}

/// A card placed on a board: the card, its board and where it sits there.
/// Borrowed from the folded views, so a list of rows costs one `Vec`.
#[derive(Clone, Copy, Debug)]
pub struct ListRow<'a> {
    /// The board the card is placed on.
    pub board: &'a BoardView,
    /// The card itself.
    pub card: &'a CardView,
    /// Where it sits on [`board`](Self::board).
    pub placement: Placement<'a>,
}

impl<'a> ListRow<'a> {
    /// The live column the card sits in, `None` when it's archived.
    pub fn column(&self) -> Option<&'a ColumnView> {
        match self.placement {
            Placement::Column(column) => Some(column),
            Placement::Archived => None,
        }
    }

    /// Whether the card is archived.
    pub fn is_archived(&self) -> bool {
        matches!(self.placement, Placement::Archived)
    }

    /// The name of where the card sits: its column's, or `archived`. What
    /// `headway list` prints in brackets, the way `grep` does.
    pub fn place_name(&self) -> &'a str {
        self.column().map_or("archived", |c| c.name.as_str())
    }

    /// Whether the card counts as done here: it sits in a terminal column, or
    /// every live subissue under it is done. The same rule as
    /// [`BoardView::card_is_done`], without re-finding the card. An archived
    /// card is neither done nor open: it's archived.
    pub fn is_done(&self) -> bool {
        self.column().is_some_and(|column| {
            self.board.column_is_terminal(&column.id) || self.card.subissues_done()
        })
    }
}

/// The lifecycle state an `is:` term asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateFilter {
    /// Not done ([`ListRow::is_done`] is false) — Linear's "Active".
    Open,
    /// Done: in a terminal column, or every subissue done.
    Done,
    /// In the board's In Progress column ([`ColumnView::is_in_progress`]).
    InProgress,
    /// Any live state. Written `is:any`; lets a caller that defaults to
    /// [`Open`](Self::Open) be told to show everything on the board.
    Any,
    /// Archived ([`Placement::Archived`]). The one state that lists archived
    /// cards: every other keeps only live ones.
    Archived,
}

impl StateFilter {
    /// Does `row` sit in this state?
    fn matches(self, row: &ListRow) -> bool {
        let Some(column) = row.column() else {
            return self == StateFilter::Archived;
        };
        match self {
            StateFilter::Open => !row.is_done(),
            StateFilter::Done => row.is_done(),
            StateFilter::InProgress => column.is_in_progress(),
            StateFilter::Any => true,
            StateFilter::Archived => false,
        }
    }
}

/// A pasted card reference, kept when the filter isn't scoped to one board: it
/// matches the card with these id words on that board.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RefTerm {
    /// The board slug, compared case-insensitively.
    board: String,
    /// The id words (`maple-river-canyon`), lowercased.
    words: String,
}

/// A parsed issue filter: an AND of constraints, each empty or `None` when not
/// given. An empty filter ([`IssueFilter::default`]) matches every card.
///
/// Built by [`IssueFilter::parse`] from a query string, or field by field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueFilter {
    /// Free-text terms (lowercased); each must appear in the card's title,
    /// description, labels or word-id.
    pub text: Vec<String>,
    /// `label:` terms (lowercased); each must be a substring of one of the
    /// card's labels.
    pub labels: Vec<String>,
    /// `priority:` values; the card's priority must be one of them.
    pub priority: Vec<Priority>,
    /// `is:open|done|progress|any|archived`.
    pub state: Option<StateFilter>,
    /// `status:`/`col:` values: the card's column id or name must equal one of
    /// them, ignoring case.
    pub columns: Vec<String>,
    /// `due:any` (`Some(true)`) or `due:none` (`Some(false)`).
    pub has_due: Option<bool>,
    /// `due:<YYYY-MM-DD>`: due on or before this day (an undated card fails).
    pub due_before: Option<Date>,
    /// `is:blocked` (`Some(true)`) or `is:unblocked` (`Some(false)`), by
    /// [`CardView::is_blocked`].
    pub blocked: Option<bool>,
    /// Drop subissues (cards with a parent) — the board's "hide sub-issues"
    /// view option. Not a query term.
    pub hide_subissues: bool,
    /// Pasted references to cards on other boards, kept only when the filter
    /// isn't scoped to one board (see [`IssueFilter::parse`]).
    refs: Vec<RefTerm>,
}

impl IssueFilter {
    /// Parse `query`. Linear-inspired: whitespace splits it into AND-ed terms,
    /// and a term is one of
    ///
    /// - `label:x` / `l:x` — some label contains `x`;
    /// - `priority:x` / `p:x` — `urgent|high|medium|low|none`, comma-separated
    ///   for any of several (`p:high,urgent`);
    /// - `is:open|done|progress|any`, `is:blocked|unblocked`;
    /// - `is:archived` — archived cards, and only them (see [`list`]);
    /// - `status:x` / `col:x` — the column, by id or name (`col:todo,backlog`);
    /// - `due:any|none|<YYYY-MM-DD>` — dated, undated, or due by that day;
    /// - anything else, free text matched against a card's title, description,
    ///   labels and its word-id (`maple-river-canyon`), so a card can be pulled
    ///   up by any of its id words.
    ///
    /// A keyed term with an empty or unknown value (`label:`, `p:soon`) is no
    /// constraint, so a half-typed term doesn't blank the list.
    ///
    /// A pasted card reference (`headway:headway/maple-river-canyon`, or the
    /// scheme-less `headway/maple-river-canyon`) depends on `board`, the slug
    /// the filter is scoped to. With `Some(board)`, a reference to that board
    /// matches by its id words — the board segment carries no signal within a
    /// single board — and other references are plain text, so a card citing an
    /// issue on another board stays searchable by that citation. With `None`
    /// (a cross-board list), a reference matches exactly the card it names.
    /// All matching is case-insensitive.
    pub fn parse(query: &str, board: Option<&str>) -> IssueFilter {
        let mut filter = IssueFilter::default();
        for term in query.split_whitespace() {
            if filter.keyed_term(term) {
                continue;
            }
            match crate::wordid::parse_ref(term) {
                // A reference to a card on *this* board: the board segment is
                // redundant here, so match on the id words alone.
                Some(r) if board.is_some_and(|b| r.board.eq_ignore_ascii_case(b)) => {
                    filter.text.push(r.words.to_lowercase());
                }
                // Unscoped: the reference names one card on one board.
                Some(r) if board.is_none() => filter.refs.push(RefTerm {
                    board: r.board.to_string(),
                    words: r.words.to_lowercase(),
                }),
                // Anything else — a reference to another board, or plain text
                // (including a bare word-id, which stays searchable via the
                // haystack) — is search text.
                _ => filter.text.push(term.to_lowercase()),
            }
        }
        filter
    }

    /// Fold a `key:value` term into the filter. `true` when `term` had a
    /// recognised key, whether or not its value made a constraint; `false`
    /// leaves it to be read as a reference or free text.
    fn keyed_term(&mut self, term: &str) -> bool {
        let Some((key, value)) = term.split_once(':') else {
            return false;
        };
        let values = || value.split(',').filter(|v| !v.is_empty());
        match key.to_ascii_lowercase().as_str() {
            "label" | "l" => {
                if !value.is_empty() {
                    self.labels.push(value.to_lowercase());
                }
            }
            "priority" | "p" => self.priority.extend(values().filter_map(priority_value)),
            "status" | "col" => self.columns.extend(values().map(str::to_string)),
            "is" => match value.to_ascii_lowercase().as_str() {
                "open" | "active" => self.state = Some(StateFilter::Open),
                "done" | "closed" => self.state = Some(StateFilter::Done),
                "progress" | "in-progress" | "started" => {
                    self.state = Some(StateFilter::InProgress)
                }
                "any" | "all" => self.state = Some(StateFilter::Any),
                "archived" => self.state = Some(StateFilter::Archived),
                "blocked" => self.blocked = Some(true),
                "unblocked" => self.blocked = Some(false),
                _ => {}
            },
            "due" => match value.to_ascii_lowercase().as_str() {
                "any" => self.has_due = Some(true),
                "none" => self.has_due = Some(false),
                day => self.due_before = Date::parse(day).or(self.due_before),
            },
            _ => return false,
        }
        true
    }

    /// Whether the filter asks for archived cards (`is:archived`), which a
    /// list leaves out otherwise.
    pub fn wants_archived(&self) -> bool {
        self.state == Some(StateFilter::Archived)
    }

    /// Whether any constraint is set. An inactive filter keeps every card.
    pub fn is_active(&self) -> bool {
        *self != IssueFilter::default()
    }

    /// Does `row` satisfy every constraint?
    pub fn matches(&self, row: &ListRow) -> bool {
        let card = row.card;
        if self.hide_subissues && card.parent.is_some() {
            return false;
        }
        if !self.priority.is_empty() && !self.priority.contains(&card.priority) {
            return false;
        }
        if self.state.is_some_and(|s| !s.matches(row)) {
            return false;
        }
        if self.blocked.is_some_and(|b| b != card.is_blocked()) {
            return false;
        }
        if !self.columns.is_empty()
            && !row.column().is_some_and(|column| {
                self.columns.iter().any(|c| {
                    c.eq_ignore_ascii_case(&column.id) || c.eq_ignore_ascii_case(&column.name)
                })
            })
        {
            return false;
        }
        if self.has_due.is_some_and(|d| d != card.due.is_some()) {
            return false;
        }
        if self
            .due_before
            .is_some_and(|by| card.due.is_none_or(|due| due > by))
        {
            return false;
        }
        let label_ok = self.labels.iter().all(|needle| {
            card.labels
                .iter()
                .any(|l| l.to_lowercase().contains(needle))
        });
        if !label_ok {
            return false;
        }
        if !self.refs.iter().all(|r| ref_matches(r, row)) {
            return false;
        }
        self.text_matches(card)
    }

    /// Does every free-text term appear somewhere in `card`?
    fn text_matches(&self, card: &CardView) -> bool {
        if self.text.is_empty() {
            return true;
        }
        let mut haystack = card.title.to_lowercase();
        haystack.push('\n');
        haystack.push_str(&card.description.to_lowercase());
        for l in &card.labels {
            haystack.push('\n');
            haystack.push_str(&l.to_lowercase());
        }
        // The card's id words (`maple-river-canyon`), already lowercase BIP-39
        // words. No board slug: in a board-scoped filter it carries no signal
        // and would make its text match every card.
        haystack.push('\n');
        haystack.push_str(&crate::wordid::encode(card.id.bytes()));
        self.text.iter().all(|needle| haystack.contains(needle))
    }
}

/// A `priority:` value, or `None` for one that names no priority (so `p:soon`
/// is ignored rather than read as "no priority").
fn priority_value(value: &str) -> Option<Priority> {
    let p = Priority::parse(value);
    (p != Priority::None || value.eq_ignore_ascii_case("none")).then_some(p)
}

/// Is `row` the card `r` names: on its board, with its id words?
fn ref_matches(r: &RefTerm, row: &ListRow) -> bool {
    r.board.eq_ignore_ascii_case(&row.board.id)
        && crate::wordid::encode(row.card.id.bytes()) == r.words
}

/// One property a list row can show beside its title — Linear's "display
/// properties". Which are on is a [`DisplayProps`] set. Written by its
/// [`name`](Self::name) in `headway list --show`/`--hide` and in a saved view.
///
/// Assignee, project and cycle join when cards carry those fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DisplayProp {
    /// The card's reference (`headway:<board>/<word-id>`, or just the word-id
    /// where the board is plain from context).
    Id,
    /// Its status: the column it sits in.
    Status,
    /// Its priority glyph.
    Priority,
    /// Its label chips.
    Labels,
    /// Its parent and its own subissues' `done/total`.
    SubIssues,
    /// How many cards block it, flagged while any is unfinished.
    Blockers,
    /// Its estimate, in points.
    Estimate,
    /// Its due date.
    Due,
    /// When it was created.
    Created,
    /// When its content last changed ([`CardView::updated_at`]).
    Updated,
    /// The board it's on.
    Board,
}

impl DisplayProp {
    /// Every property, in the order a row draws them left to right and a
    /// toggle menu lists them.
    pub const ALL: [DisplayProp; 11] = [
        DisplayProp::Priority,
        DisplayProp::Id,
        DisplayProp::Status,
        DisplayProp::SubIssues,
        DisplayProp::Labels,
        DisplayProp::Estimate,
        DisplayProp::Blockers,
        DisplayProp::Due,
        DisplayProp::Created,
        DisplayProp::Updated,
        DisplayProp::Board,
    ];

    /// The property's written name: what `--show`/`--hide` and
    /// [`FromStr`](std::str::FromStr) read, and the serde form.
    pub fn name(self) -> &'static str {
        match self {
            DisplayProp::Id => "id",
            DisplayProp::Status => "status",
            DisplayProp::Priority => "priority",
            DisplayProp::Labels => "labels",
            DisplayProp::SubIssues => "subissues",
            DisplayProp::Blockers => "blockers",
            DisplayProp::Estimate => "estimate",
            DisplayProp::Due => "due",
            DisplayProp::Created => "created",
            DisplayProp::Updated => "updated",
            DisplayProp::Board => "board",
        }
    }

    /// The property's name for a person, as a toggle chip shows it.
    pub fn label(self) -> &'static str {
        match self {
            DisplayProp::Id => "ID",
            DisplayProp::Status => "Status",
            DisplayProp::Priority => "Priority",
            DisplayProp::Labels => "Labels",
            DisplayProp::SubIssues => "Sub-issues",
            DisplayProp::Blockers => "Blockers",
            DisplayProp::Estimate => "Estimate",
            DisplayProp::Due => "Due date",
            DisplayProp::Created => "Created",
            DisplayProp::Updated => "Updated",
            DisplayProp::Board => "Board",
        }
    }

    /// This property's bit in a [`DisplayProps`].
    fn bit(self) -> u16 {
        1 << self as u16
    }
}

impl std::fmt::Display for DisplayProp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A name that is no [`DisplayProp`]'s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownDisplayProp(pub String);

impl std::fmt::Display for UnknownDisplayProp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown display property `{}` (expected one of ", self.0)?;
        for (i, prop) in DisplayProp::ALL.iter().enumerate() {
            let sep = if i == 0 { "" } else { ", " };
            write!(f, "{sep}{prop}")?;
        }
        f.write_str(")")
    }
}

impl std::error::Error for UnknownDisplayProp {}

impl std::str::FromStr for DisplayProp {
    type Err = UnknownDisplayProp;

    /// A property by its [`name`](DisplayProp::name), ignoring case and
    /// surrounding space. `sub-issues` and `blocked` are read too, as people
    /// write them.
    fn from_str(s: &str) -> Result<DisplayProp, UnknownDisplayProp> {
        let name = s.trim().to_ascii_lowercase();
        let alias = match name.as_str() {
            "sub-issues" | "subissue" => "subissues",
            "blocked" | "blocker" => "blockers",
            "column" | "col" => "status",
            "ref" => "id",
            other => other,
        };
        DisplayProp::ALL
            .into_iter()
            .find(|prop| prop.name() == alias)
            .ok_or(UnknownDisplayProp(s.trim().to_string()))
    }
}

/// Which [`DisplayProp`]s a list row shows: a set, as bits, so asking costs
/// nothing per frame. [`Default`] is Linear's: everything but created and
/// board.
///
/// Serializes as the list of its properties' names, so a saved view reads
/// `["priority","id",…]` and survives a property being added.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(into = "Vec<DisplayProp>", from = "Vec<DisplayProp>")]
pub struct DisplayProps(u16);

impl Default for DisplayProps {
    fn default() -> DisplayProps {
        DisplayProps::of(&[
            DisplayProp::Priority,
            DisplayProp::Id,
            DisplayProp::Status,
            DisplayProp::Labels,
            DisplayProp::SubIssues,
            DisplayProp::Blockers,
            DisplayProp::Estimate,
            DisplayProp::Due,
            DisplayProp::Updated,
        ])
    }
}

impl DisplayProps {
    /// The empty set: a row of just titles.
    pub const NONE: DisplayProps = DisplayProps(0);

    /// The set holding exactly `props`.
    pub fn of(props: &[DisplayProp]) -> DisplayProps {
        let mut set = DisplayProps::NONE;
        for &prop in props {
            set.set(prop, true);
        }
        set
    }

    /// Whether `prop` is shown.
    pub fn contains(self, prop: DisplayProp) -> bool {
        self.0 & prop.bit() != 0
    }

    /// Show (`on`) or hide `prop`.
    pub fn set(&mut self, prop: DisplayProp, on: bool) {
        if on {
            self.0 |= prop.bit();
        } else {
            self.0 &= !prop.bit();
        }
    }

    /// Flip whether `prop` is shown.
    pub fn toggle(&mut self, prop: DisplayProp) {
        self.set(prop, !self.contains(prop));
    }

    /// The shown properties, in [`DisplayProp::ALL`]'s order.
    pub fn iter(self) -> impl Iterator<Item = DisplayProp> {
        DisplayProp::ALL
            .into_iter()
            .filter(move |&prop| self.contains(prop))
    }

    /// Parse a comma-separated list of names (`labels,due`) into the set of
    /// them. An empty list is the empty set.
    pub fn parse_list(list: &str) -> Result<DisplayProps, UnknownDisplayProp> {
        let mut set = DisplayProps::NONE;
        for name in list.split(',').filter(|n| !n.trim().is_empty()) {
            set.set(name.parse()?, true);
        }
        Ok(set)
    }

    /// The set `headway list --show`/`--hide` ask for: `show` when given,
    /// else the [default](Self::default), less anything in `hide`.
    pub fn shown_hidden(show: Option<DisplayProps>, hide: DisplayProps) -> DisplayProps {
        DisplayProps(show.unwrap_or_default().0 & !hide.0)
    }
}

impl From<DisplayProps> for Vec<DisplayProp> {
    fn from(set: DisplayProps) -> Vec<DisplayProp> {
        set.iter().collect()
    }
}

impl From<Vec<DisplayProp>> for DisplayProps {
    fn from(props: Vec<DisplayProp>) -> DisplayProps {
        DisplayProps::of(&props)
    }
}

/// What a list's rows are gathered under — Linear's "Grouping" and
/// "Sub-grouping". Written by its [`name`](Self::name) in `headway list
/// --group` and in a saved view.
///
/// Assignee, project and cycle join when cards carry those fields.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum GroupBy {
    /// No groups: one flat list.
    #[default]
    None,
    /// By column, matched by name across boards, terminal columns last.
    Status,
    /// By priority, Urgent first and "No priority" last.
    Priority,
    /// By label: a card with several labels is in each of their groups.
    Label,
    /// By the board a card is on.
    Board,
    /// By the card's parent, top-level cards under "No parent".
    Parent,
}

impl GroupBy {
    /// Every grouping, in the order a menu offers them.
    pub const ALL: [GroupBy; 6] = [
        GroupBy::None,
        GroupBy::Status,
        GroupBy::Priority,
        GroupBy::Label,
        GroupBy::Board,
        GroupBy::Parent,
    ];

    /// The grouping's written name: what `--group` and
    /// [`FromStr`](std::str::FromStr) read, and the serde form.
    pub fn name(self) -> &'static str {
        match self {
            GroupBy::None => "none",
            GroupBy::Status => "status",
            GroupBy::Priority => "priority",
            GroupBy::Label => "label",
            GroupBy::Board => "board",
            GroupBy::Parent => "parent",
        }
    }

    /// The grouping's name for a person, as a menu shows it.
    pub fn label(self) -> &'static str {
        match self {
            GroupBy::None => "No grouping",
            GroupBy::Status => "Status",
            GroupBy::Priority => "Priority",
            GroupBy::Label => "Label",
            GroupBy::Board => "Board",
            GroupBy::Parent => "Parent",
        }
    }

    /// The display property a group header already says, so a row under it
    /// needn't: grouped by status, every row in a group has its status.
    pub fn implied_prop(self) -> Option<DisplayProp> {
        match self {
            GroupBy::Status => Some(DisplayProp::Status),
            GroupBy::Priority => Some(DisplayProp::Priority),
            GroupBy::Label => Some(DisplayProp::Labels),
            GroupBy::Board => Some(DisplayProp::Board),
            GroupBy::None | GroupBy::Parent => None,
        }
    }
}

impl std::fmt::Display for GroupBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A name that is no [`GroupBy`]'s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownGroupBy(pub String);

impl std::fmt::Display for UnknownGroupBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown grouping `{}` (expected one of ", self.0)?;
        for (i, by) in GroupBy::ALL.iter().enumerate() {
            let sep = if i == 0 { "" } else { ", " };
            write!(f, "{sep}{by}")?;
        }
        f.write_str(")")
    }
}

impl std::error::Error for UnknownGroupBy {}

impl std::str::FromStr for GroupBy {
    type Err = UnknownGroupBy;

    /// A grouping by its [`name`](GroupBy::name), ignoring case and
    /// surrounding space, plus the plurals and synonyms people write
    /// (`labels`, `column`, `epic`).
    fn from_str(s: &str) -> Result<GroupBy, UnknownGroupBy> {
        let name = s.trim().to_ascii_lowercase();
        let alias = match name.as_str() {
            "labels" => "label",
            "column" | "col" => "status",
            "epic" => "parent",
            "boards" => "board",
            "" | "no" => "none",
            other => other,
        };
        GroupBy::ALL
            .into_iter()
            .find(|by| by.name() == alias)
            .ok_or(UnknownGroupBy(s.trim().to_string()))
    }
}

/// How a list gathers its rows: under [`by`](Self::by)'s groups, each split
/// again by [`sub`](Self::sub)'s. The default is no grouping, the flat list.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(default)]
pub struct Grouping {
    /// The top-level groups.
    pub by: GroupBy,
    /// The groups inside each top-level group. Ignored while
    /// [`by`](Self::by) is [`GroupBy::None`] or the same as it.
    pub sub: GroupBy,
    /// Also draw the groups no row falls in: every column (status), every
    /// priority, every board. Label and parent groups exist only where a row
    /// does.
    pub show_empty: bool,
}

impl Grouping {
    /// Whether the list is grouped at all.
    pub fn is_grouped(self) -> bool {
        self.by != GroupBy::None
    }

    /// The sub-grouping that applies: [`sub`](Self::sub), unless the list
    /// isn't grouped or `sub` repeats [`by`](Self::by).
    pub fn effective_sub(self) -> GroupBy {
        if self.is_grouped() && self.sub != self.by {
            self.sub
        } else {
            GroupBy::None
        }
    }

    /// `display` less what the group headers already say (see
    /// [`GroupBy::implied_prop`]), the set a grouped row draws.
    pub fn row_display(self, mut display: DisplayProps) -> DisplayProps {
        for by in [self.by, self.effective_sub()] {
            if let Some(prop) = by.implied_prop() {
                display.set(prop, false);
            }
        }
        display
    }
}

/// Which group a [`ListGroup`] is: the value its rows share. A front end reads
/// it to make a card that belongs there (a group's `+`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GroupKey {
    /// The single group of an ungrouped list.
    All,
    /// Cards in a column of this name, lowercased: the same-named columns of
    /// every board are one group.
    Status(String),
    /// Archived cards, in no column (listed only for `is:archived`).
    Archived,
    /// Cards of this priority.
    Priority(Priority),
    /// Cards with this label, lowercased; `None` for unlabelled cards.
    Label(Option<String>),
    /// Cards on this board.
    Board {
        /// The board's owner.
        author: [u8; 32],
        /// The board's slug.
        id: String,
    },
    /// Subissues of this card; `None` for top-level cards.
    Parent(Option<NoteId>),
}

/// One group of a grouped list ([`group`]): which group it is, the header it
/// draws, and its rows, as indices into the rows that were grouped.
///
/// [`rows`](Self::rows) is every row in the group, sub-grouped or not, so its
/// length is the header's count. [`subgroups`](Self::subgroups) splits them
/// again when a sub-grouping is set; a row with two labels is in two label
/// subgroups, so they can hold more than `rows` does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListGroup {
    /// The value its rows share.
    pub key: GroupKey,
    /// The header text: a column's name, `High`, `No label`, a board's
    /// title, a parent's title.
    pub label: String,
    /// Its rows, as indices into the slice [`group`] was handed, in that
    /// slice's order.
    pub rows: Vec<usize>,
    /// Its sub-groups, empty unless a sub-grouping applies.
    pub subgroups: Vec<ListGroup>,
}

/// Gather `rows` under `grouping`'s groups, in the order a list draws them.
/// `boards` are the boards the rows were listed from, which say what empty
/// groups exist ([`Grouping::show_empty`]) and in what order the columns go.
///
/// Orders: status follows the boards' columns, board by board, a name joining
/// at its first appearance, terminal columns last and archived after them;
/// priority runs Urgent to No priority; labels go alphabetically, "No label"
/// last; boards keep `boards`' order; parents go in the order they're first
/// met, "No parent" last. Ungrouped ([`GroupBy::None`]), it is one
/// [`GroupKey::All`] group of every row.
pub fn group(boards: &[&BoardView], rows: &[ListRow], grouping: Grouping) -> Vec<ListGroup> {
    let all: Vec<usize> = (0..rows.len()).collect();
    if !grouping.is_grouped() {
        return vec![ListGroup {
            key: GroupKey::All,
            label: "All issues".to_string(),
            rows: all,
            subgroups: Vec::new(),
        }];
    }
    let mut groups = group_by(boards, rows, &all, grouping.by, grouping.show_empty);
    let sub = grouping.effective_sub();
    if sub != GroupBy::None {
        for g in &mut groups {
            g.subgroups = group_by(boards, rows, &g.rows, sub, grouping.show_empty);
        }
    }
    groups
}

/// The `of` rows (indices into `rows`) gathered under `by`'s groups, in
/// [`group`]'s order, the empty ones kept when `show_empty`.
fn group_by(
    boards: &[&BoardView],
    rows: &[ListRow],
    of: &[usize],
    by: GroupBy,
    show_empty: bool,
) -> Vec<ListGroup> {
    let mut groups = seed_groups(boards, by);
    for &at in of {
        for found in row_groups(&rows[at], by) {
            let slot = match groups.iter().position(|g| g.key == found.key) {
                Some(slot) => slot,
                None => {
                    groups.push(found);
                    groups.len() - 1
                }
            };
            groups[slot].rows.push(at);
        }
    }
    match by {
        // Seeded in order; an archived group joined last, after them.
        GroupBy::Status | GroupBy::Priority | GroupBy::Board | GroupBy::None => {}
        GroupBy::Label => groups.sort_by(|a, b| match (&a.key, &b.key) {
            (GroupKey::Label(Some(a)), GroupKey::Label(Some(b))) => a.cmp(b),
            (GroupKey::Label(None), _) => std::cmp::Ordering::Greater,
            (_, GroupKey::Label(None)) => std::cmp::Ordering::Less,
            _ => std::cmp::Ordering::Equal,
        }),
        GroupBy::Parent => groups.sort_by_key(|g| g.key == GroupKey::Parent(None)),
    }
    if !show_empty {
        groups.retain(|g| !g.rows.is_empty());
    }
    groups
}

/// The groups `by` has whether or not a row falls in them, empty and in
/// order: every column name across `boards` (terminal ones last), every
/// priority, every board. Label and parent groups come only from rows.
fn seed_groups(boards: &[&BoardView], by: GroupBy) -> Vec<ListGroup> {
    let empty = |key, label: &str| ListGroup {
        key,
        label: label.to_string(),
        rows: Vec::new(),
        subgroups: Vec::new(),
    };
    match by {
        GroupBy::Status => {
            let mut open = Vec::new();
            let mut terminal = Vec::new();
            for board in boards {
                for column in &board.columns {
                    let key = GroupKey::Status(column.name.to_lowercase());
                    let seen = |g: &ListGroup| g.key == key;
                    if open.iter().any(seen) || terminal.iter().any(seen) {
                        continue;
                    }
                    let side = if board.column_is_terminal(&column.id) {
                        &mut terminal
                    } else {
                        &mut open
                    };
                    side.push(empty(key, &column.name));
                }
            }
            open.extend(terminal);
            open
        }
        GroupBy::Priority => PRIORITY_ORDER
            .into_iter()
            .map(|p| empty(GroupKey::Priority(p), priority_group_label(p)))
            .collect(),
        GroupBy::Board => boards
            .iter()
            .map(|b| {
                let key = GroupKey::Board {
                    author: b.author,
                    id: b.id.clone(),
                };
                empty(key, &b.title)
            })
            .collect(),
        GroupBy::None | GroupBy::Label | GroupBy::Parent => Vec::new(),
    }
}

/// Priorities in group order: Linear's, most urgent first.
const PRIORITY_ORDER: [Priority; 5] = [
    Priority::Urgent,
    Priority::High,
    Priority::Medium,
    Priority::Low,
    Priority::None,
];

/// A priority group's header.
fn priority_group_label(p: Priority) -> &'static str {
    match p {
        Priority::Urgent => "Urgent",
        Priority::High => "High",
        Priority::Medium => "Medium",
        Priority::Low => "Low",
        Priority::None => "No priority",
    }
}

/// The groups `row` falls in under `by`, still empty: one, or for a card
/// with several labels one per label.
fn row_groups(row: &ListRow, by: GroupBy) -> Vec<ListGroup> {
    let card = row.card;
    let one = |key, label: String| {
        vec![ListGroup {
            key,
            label,
            rows: Vec::new(),
            subgroups: Vec::new(),
        }]
    };
    match by {
        GroupBy::None => one(GroupKey::All, "All issues".to_string()),
        GroupBy::Status => match row.column() {
            Some(column) => one(
                GroupKey::Status(column.name.to_lowercase()),
                column.name.clone(),
            ),
            None => one(GroupKey::Archived, "Archived".to_string()),
        },
        GroupBy::Priority => one(
            GroupKey::Priority(card.priority),
            priority_group_label(card.priority).to_string(),
        ),
        GroupBy::Label => {
            let mut groups: Vec<ListGroup> = Vec::new();
            for label in &card.labels {
                let key = GroupKey::Label(Some(label.to_lowercase()));
                if !groups.iter().any(|g| g.key == key) {
                    groups.extend(one(key, label.clone()));
                }
            }
            if groups.is_empty() {
                return one(GroupKey::Label(None), "No label".to_string());
            }
            groups
        }
        GroupBy::Board => one(
            GroupKey::Board {
                author: row.board.author,
                id: row.board.id.clone(),
            },
            row.board.title.clone(),
        ),
        GroupBy::Parent => match card.parent {
            Some(parent) => {
                let label = row.board.card(parent).map_or_else(
                    || crate::wordid::encode(parent.bytes()),
                    |p| p.title.clone(),
                );
                one(GroupKey::Parent(Some(parent)), label)
            }
            None => one(GroupKey::Parent(None), "No parent".to_string()),
        },
    }
}

/// What a list's rows are ordered by — Linear's "Ordering". Written by its
/// [`name`](Self::name) in `headway list --sort` and in a saved view.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum SortBy {
    /// The boards' own order: board by board, column by column, each column
    /// by rank — the board as drawn.
    #[default]
    Manual,
    /// By priority, cards with none last whichever way it runs; ties go to
    /// the most recently updated.
    Priority,
    /// By when the card's content last changed ([`CardView::updated_at`]).
    Updated,
    /// By when the card was made.
    Created,
    /// By due date, undated cards last whichever way it runs.
    Due,
    /// Each board's work-order ([`crate::traversal::work_order`] over its
    /// root): what `headway next` walks, sequenced cards first, each followed
    /// by its subissues. Cards outside it (archived) go last.
    WorkOrder,
}

impl SortBy {
    /// Every ordering, in the order a menu offers them.
    pub const ALL: [SortBy; 6] = [
        SortBy::Manual,
        SortBy::Priority,
        SortBy::Updated,
        SortBy::Created,
        SortBy::Due,
        SortBy::WorkOrder,
    ];

    /// The ordering's written name: what `--sort` and
    /// [`FromStr`](std::str::FromStr) read, and the serde form.
    pub fn name(self) -> &'static str {
        match self {
            SortBy::Manual => "manual",
            SortBy::Priority => "priority",
            SortBy::Updated => "updated",
            SortBy::Created => "created",
            SortBy::Due => "due",
            SortBy::WorkOrder => "work-order",
        }
    }

    /// The ordering's name for a person, as a menu shows it.
    pub fn label(self) -> &'static str {
        match self {
            SortBy::Manual => "Manual",
            SortBy::Priority => "Priority",
            SortBy::Updated => "Last updated",
            SortBy::Created => "Created",
            SortBy::Due => "Due date",
            SortBy::WorkOrder => "Work order",
        }
    }

    /// The direction the ordering naturally runs: most urgent, newest first;
    /// soonest due, board and work order top first.
    pub fn natural_descending(self) -> bool {
        match self {
            SortBy::Priority | SortBy::Updated | SortBy::Created => true,
            SortBy::Manual | SortBy::Due | SortBy::WorkOrder => false,
        }
    }
}

impl std::fmt::Display for SortBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A name that is no [`SortBy`]'s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownSortBy(pub String);

impl std::fmt::Display for UnknownSortBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown ordering `{}` (expected one of ", self.0)?;
        for (i, by) in SortBy::ALL.iter().enumerate() {
            let sep = if i == 0 { "" } else { ", " };
            write!(f, "{sep}{by}")?;
        }
        f.write_str(")")
    }
}

impl std::error::Error for UnknownSortBy {}

impl std::str::FromStr for SortBy {
    type Err = UnknownSortBy;

    /// An ordering by its [`name`](SortBy::name), ignoring case and
    /// surrounding space, plus the synonyms people write (`board`, `seq`,
    /// `due-date`).
    fn from_str(s: &str) -> Result<SortBy, UnknownSortBy> {
        let name = s.trim().to_ascii_lowercase();
        let alias = match name.as_str() {
            "board" | "rank" => "manual",
            "work" | "workorder" | "work_order" | "seq" | "sequence" | "next" => "work-order",
            "due-date" | "due_date" => "due",
            "updated-at" | "modified" => "updated",
            "created-at" | "new" | "newest" => "created",
            other => other,
        };
        SortBy::ALL
            .into_iter()
            .find(|by| by.name() == alias)
            .ok_or(UnknownSortBy(s.trim().to_string()))
    }
}

/// How a list orders its rows: by [`by`](Self::by), which way, and whether
/// done rows go last by recency. Applied before grouping, so every group's
/// rows come out in this order ([`group`] keeps the order it's handed). The
/// default is [`SortBy::Manual`], the boards' own order.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(default)]
pub struct Sorting {
    /// What the rows are ordered by.
    pub by: SortBy,
    /// Run the ordering high to low: Urgent first, newest first, latest due
    /// first, the board or work order bottom first. Cards with no priority
    /// or no due date stay last either way.
    pub descending: bool,
    /// Linear's "order completed by recency": done rows go after the open
    /// ones, most recently updated first, whatever [`by`](Self::by) is.
    pub completed_by_recency: bool,
}

impl Sorting {
    /// Ordering by `by`, run its natural way ([`SortBy::natural_descending`]).
    pub fn by(by: SortBy) -> Sorting {
        Sorting {
            by,
            descending: by.natural_descending(),
            completed_by_recency: false,
        }
    }
}

/// The order `sorting` puts `rows` in, as indices into them: `rows[order[0]]`
/// is first. Stable, so rows the ordering doesn't tell apart keep the order
/// they came in (the boards' own, from [`list`]).
pub fn sort_order(rows: &[ListRow], sorting: Sorting) -> Vec<usize> {
    use std::cmp::Ordering;

    let mut order: Vec<usize> = (0..rows.len()).collect();
    let work = (sorting.by == SortBy::WorkOrder).then(|| WorkPositions::of(rows));
    // `ord` read the chosen way; `last` puts a missing value at the end
    // whichever way it runs.
    let dir = |ord: Ordering| {
        if sorting.descending {
            ord.reverse()
        } else {
            ord
        }
    };
    order.sort_by(|&a, &b| {
        let (ra, rb) = (&rows[a], &rows[b]);
        if sorting.completed_by_recency {
            match (ra.is_done(), rb.is_done()) {
                (true, true) => return rb.card.updated_at.cmp(&ra.card.updated_at),
                (false, true) => return Ordering::Less,
                (true, false) => return Ordering::Greater,
                (false, false) => {}
            }
        }
        let (ca, cb) = (ra.card, rb.card);
        match sorting.by {
            SortBy::Manual => dir(a.cmp(&b)),
            SortBy::Priority => last_if_none(
                Some(ca.priority).filter(|p| *p != Priority::None),
                Some(cb.priority).filter(|p| *p != Priority::None),
                dir,
            )
            .then_with(|| cb.updated_at.cmp(&ca.updated_at)),
            SortBy::Updated => dir(ca.updated_at.cmp(&cb.updated_at)),
            SortBy::Created => dir(ca.created_at.cmp(&cb.created_at)),
            SortBy::Due => last_if_none(ca.due, cb.due, dir),
            SortBy::WorkOrder => {
                let Some(work) = &work else {
                    return Ordering::Equal;
                };
                let (wa, wb) = (work.place(ra), work.place(rb));
                wa.board
                    .cmp(&wb.board)
                    .then_with(|| last_if_none(wa.at, wb.at, dir))
            }
        }
    });
    order
}

/// Compare `a` and `b` through `dir`, a missing one last either way.
fn last_if_none<T: Ord>(
    a: Option<T>,
    b: Option<T>,
    dir: impl Fn(std::cmp::Ordering) -> std::cmp::Ordering,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Some(a), Some(b)) => dir(a.cmp(&b)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Where each row's card sits in its board's work-order, for
/// [`SortBy::WorkOrder`]: the boards the rows are on, in the order the rows
/// first meet them, each with its work-order positions.
struct WorkPositions<'a> {
    boards: Vec<BoardOrder<'a>>,
}

/// One board's work-order, as [`WorkPositions`] keeps it.
struct BoardOrder<'a> {
    board: &'a BoardView,
    /// Each card's place in the walk, by id.
    at: HashMap<[u8; 32], usize>,
}

/// A row's place for [`SortBy::WorkOrder`]: its board's turn, and where in
/// that board's walk its card is (`None` for a card the walk never reaches).
struct WorkPlace {
    board: usize,
    at: Option<usize>,
}

impl<'a> WorkPositions<'a> {
    /// The work-orders of every board `rows` are on.
    fn of(rows: &[ListRow<'a>]) -> WorkPositions<'a> {
        let mut boards: Vec<BoardOrder<'a>> = Vec::new();
        for row in rows {
            if boards.iter().any(|b| std::ptr::eq(b.board, row.board)) {
                continue;
            }
            let root = crate::event::Container::BoardRoot(row.board.id.clone());
            let at = crate::traversal::work_order(row.board, &root)
                .into_iter()
                .enumerate()
                .map(|(i, card)| (*card.id.bytes(), i))
                .collect();
            boards.push(BoardOrder {
                board: row.board,
                at,
            });
        }
        WorkPositions { boards }
    }

    /// Where `row` goes.
    fn place(&self, row: &ListRow) -> WorkPlace {
        let board = self
            .boards
            .iter()
            .position(|b| std::ptr::eq(b.board, row.board))
            .unwrap_or(self.boards.len());
        let at = self
            .boards
            .get(board)
            .and_then(|b| b.at.get(row.card.id.bytes()).copied());
        WorkPlace { board, at }
    }
}

/// The cards among `rows` that are in their board's ready frontier
/// ([`crate::traversal::ready`] over its root): what `headway next --ready`
/// hands out. A list in [`SortBy::WorkOrder`] marks them.
pub fn ready_set(rows: &[ListRow]) -> HashSet<[u8; 32]> {
    let mut seen: Vec<&BoardView> = Vec::new();
    let mut ready = HashSet::new();
    for row in rows {
        if seen.iter().any(|b| std::ptr::eq(*b, row.board)) {
            continue;
        }
        seen.push(row.board);
        let root = crate::event::Container::BoardRoot(row.board.id.clone());
        ready.extend(
            crate::traversal::ready(row.board, &root)
                .into_iter()
                .map(|card| *card.id.bytes()),
        );
    }
    ready
}

/// A list view as it's saved: its name, its query and how its rows are
/// drawn, ordered and gathered — Linear's saved views, and the tabs above the
/// list. Persisted as JSON in a kind-30626 note ([`crate::views`]); the
/// filter is kept as its query *text*, read with [`IssueFilter::parse`], so
/// the stored form stays readable and survives the filter's fields changing.
/// Every field defaults when missing, so a view saved before a field existed
/// still loads.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ViewSpec {
    /// The view's name, as its tab shows it.
    pub name: String,
    /// The filter's query text (`p:high label:ui`). Without an `is:` term the
    /// view keeps open cards, as `headway list` does.
    pub query: String,
    /// How the rows are gathered under headers.
    pub grouping: Grouping,
    /// How the rows are ordered.
    pub sorting: Sorting,
    /// Which properties a row shows.
    pub display: DisplayProps,
    /// Drop subissues, keeping top-level cards only
    /// ([`IssueFilter::hide_subissues`]).
    pub hide_subissues: bool,
    /// The board slug the view is scoped to; `None` looks at every board.
    pub board: Option<String>,
}

impl ViewSpec {
    /// The view's filter: its query parsed (scoped to its board when it has
    /// one), subissues dropped if it says so, and open cards only when the
    /// query names no state.
    pub fn filter(&self) -> IssueFilter {
        let mut filter = IssueFilter::parse(&self.query, self.board.as_deref());
        filter.hide_subissues = self.hide_subissues;
        if filter.state.is_none() {
            filter.state = Some(StateFilter::Open);
        }
        filter
    }
}

/// The views every list has without saving one — the tabs Linear opens on:
/// Active, Backlog and All issues. Saved views come after them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuiltinView {
    /// The open cards.
    Active,
    /// The cards in a column named Backlog.
    Backlog,
    /// Every card, done ones too.
    All,
}

impl BuiltinView {
    /// Every built-in view, in tab order.
    pub const ALL: [BuiltinView; 3] = [BuiltinView::Active, BuiltinView::Backlog, BuiltinView::All];

    /// The view's name, as its tab shows it.
    pub fn name(self) -> &'static str {
        match self {
            BuiltinView::Active => "Active",
            BuiltinView::Backlog => "Backlog",
            BuiltinView::All => "All issues",
        }
    }

    /// The view's query: what it narrows the list to.
    pub fn query(self) -> &'static str {
        match self {
            BuiltinView::Active => "is:open",
            BuiltinView::Backlog => "col:backlog",
            BuiltinView::All => "is:any",
        }
    }

    /// The built-in view `name` names, ignoring case, space and dashes
    /// (`all`, `all-issues`, `All issues`).
    pub fn named(name: &str) -> Option<BuiltinView> {
        let squash = |s: &str| -> String {
            s.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect()
        };
        let want = squash(name);
        BuiltinView::ALL
            .into_iter()
            .find(|v| squash(v.name()) == want || (want == "all" && *v == BuiltinView::All))
    }

    /// The view laid out as `layout` is: its name and query over `layout`'s
    /// grouping, ordering and display. A built-in carries no layout of its
    /// own, so each front end lays it out its default way.
    pub fn spec(self, layout: &ViewSpec) -> ViewSpec {
        ViewSpec {
            name: self.name().to_string(),
            query: self.query().to_string(),
            ..layout.clone()
        }
    }
}

/// What a list shows: the [`IssueFilter`], where to look, and how its rows
/// are drawn, ordered and gathered.
#[derive(Clone, Debug, Default)]
pub struct ListQuery {
    /// Which cards to keep.
    pub filter: IssueFilter,
    /// Only this card and its descendants (`--in <card>`), on whichever board
    /// holds it. `None` looks at every card.
    pub within: Option<[u8; 32]>,
    /// Only the board with this slug: a saved view's board scope
    /// ([`ViewSpec::board`]). `None` looks at every board handed in.
    pub board: Option<String>,
    /// Which properties each row shows beside its title. Rendering only:
    /// [`list`] keeps the same rows whatever it holds.
    pub display: DisplayProps,
    /// How the rows are gathered under headers ([`group`]). [`list`] keeps
    /// the same rows whatever it holds.
    pub grouping: Grouping,
    /// The order [`list`] hands its rows back in.
    pub sorting: Sorting,
}

/// Where a card sits on its board, by index: the lifetime-free form of a
/// [`Placement`], for a caller that keeps rows across frames (the GUI list)
/// and reads them back against the boards with [`RowPos::row`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowSlot {
    /// `columns[col].cards[card]`.
    Column { col: usize, card: usize },
    /// `archived[at]`: listed only for an `is:archived` query.
    Archived(usize),
}

impl RowSlot {
    /// The card this slot names on `board`.
    pub fn card(self, board: &BoardView) -> &CardView {
        match self {
            RowSlot::Column { col, card } => &board.columns[col].cards[card],
            RowSlot::Archived(at) => &board.archived[at].card,
        }
    }

    /// Where the card sits on `board`, as [`ListRow`] reads it.
    pub fn placement(self, board: &BoardView) -> Placement<'_> {
        match self {
            RowSlot::Column { col, .. } => Placement::Column(&board.columns[col]),
            RowSlot::Archived(_) => Placement::Archived,
        }
    }
}

/// A row [`list_positions`] keeps, by index: which of the boards it was handed
/// the card is on, and where on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowPos {
    /// Index of the card's board among the boards the walk was handed.
    pub board: usize,
    /// Where the card sits on that board.
    pub slot: RowSlot,
}

impl RowPos {
    /// The row this position names, read against `board` — the one at
    /// [`self.board`](Self::board) among the boards it was computed from.
    pub fn row(self, board: &BoardView) -> ListRow<'_> {
        ListRow {
            board,
            card: self.slot.card(board),
            placement: self.slot.placement(board),
        }
    }
}

/// Every card on `boards` that `query` keeps, in [`query.sorting`](ListQuery::sorting)'s
/// order, as positions into `boards`. That starts from board order — board by
/// board, column by column, each column by rank, the board as drawn — which
/// [`SortBy::Manual`] keeps and every other ordering breaks ties by. Archived
/// cards are listed only when the filter asks for them
/// ([`IssueFilter::wants_archived`]), after each board's live ones, so a
/// default list never changes for them. A board other than
/// [`query.board`](ListQuery::board) is skipped, but still counts for the
/// indices.
///
/// The one walk every list runs: [`list`] reads the positions back as rows,
/// and the GUI keeps them across frames against its own board handles.
pub fn list_positions<'a>(
    boards: impl IntoIterator<Item = &'a BoardView>,
    query: &ListQuery,
) -> Vec<RowPos> {
    // Index for index: each kept position and the row it reads as.
    let mut kept: Vec<RowPos> = Vec::new();
    let mut rows: Vec<ListRow<'a>> = Vec::new();
    for (b, board) in boards.into_iter().enumerate() {
        if query.board.as_ref().is_some_and(|slug| *slug != board.id) {
            continue;
        }
        let within = query.within.map(|root| subtree_ids(board, root));
        let mut keep = |slot: RowSlot| {
            let pos = RowPos { board: b, slot };
            let row = pos.row(board);
            let inside = within
                .as_ref()
                .is_none_or(|ids| ids.contains(row.card.id.bytes()));
            if inside && query.filter.matches(&row) {
                kept.push(pos);
                rows.push(row);
            }
        };
        for (col, column) in board.columns.iter().enumerate() {
            for card in 0..column.cards.len() {
                keep(RowSlot::Column { col, card });
            }
        }
        if !query.filter.wants_archived() {
            continue;
        }
        for at in 0..board.archived.len() {
            keep(RowSlot::Archived(at));
        }
    }
    if query.sorting == Sorting::default() {
        return kept;
    }
    sort_order(&rows, query.sorting)
        .into_iter()
        .map(|at| kept[at])
        .collect()
}

/// Every card on `boards` that `query` keeps, as rows, in the order
/// [`list_positions`] gives.
pub fn list<'a>(boards: &'a [BoardView], query: &ListQuery) -> Vec<ListRow<'a>> {
    list_positions(boards, query)
        .into_iter()
        .map(|pos| pos.row(&boards[pos.board]))
        .collect()
}

/// `root` and every card below it on `view`, live or archived — the set an
/// `--in <card>` scope keeps.
///
/// The container itself is in the set: "list this epic" or "search this epic"
/// means the epic too. A subissue cycle that slipped past the write-time guard
/// terminates here, since a card is visited once. A `root` not on `view`
/// yields just itself, which matches nothing there.
pub fn subtree_ids(view: &BoardView, root: [u8; 32]) -> HashSet<[u8; 32]> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{ArchivedCard, EdgeRef, SubissueView};
    use nostrdb_net::NoteId;

    fn card(n: u8, title: &str, labels: &[&str]) -> CardView {
        CardView {
            id: NoteId::new([n; 32]),
            author: [0; 32],
            title: title.to_string(),
            description: String::new(),
            labels: labels.iter().map(|l| l.to_string()).collect(),
            priority: Priority::None,
            due: None,
            estimate: None,
            rank: "m".to_string(),
            seq: None,
            placed_at: 0,
            created_at: 0,
            updated_at: 0,
            comments: vec![],
            reviews: vec![],
            activity: vec![],
            parent: None,
            subissues: vec![],
            blocked_by: vec![],
            blocks: vec![],
            related: vec![],
        }
    }

    fn column(id: &str, name: &str, terminal: bool, cards: Vec<CardView>) -> ColumnView {
        ColumnView {
            id: id.to_string(),
            name: name.to_string(),
            terminal,
            cards,
        }
    }

    /// A board `slug` of Todo / In Progress / Done (terminal) columns.
    fn board(
        slug: &str,
        todo: Vec<CardView>,
        doing: Vec<CardView>,
        done: Vec<CardView>,
    ) -> BoardView {
        BoardView {
            id: slug.to_string(),
            author: [0; 32],
            title: slug.to_string(),
            description: String::new(),
            created_at: 0,
            columns: vec![
                column("todo", "Todo", false, todo),
                column("in-progress", "In Progress", false, doing),
                column("done", "Done", true, done),
            ],
            archived: vec![],
            deleted: false,
        }
    }

    /// The titles `query` keeps on `boards`, in list order.
    fn titles(boards: &[BoardView], query: &str) -> Vec<String> {
        let query = ListQuery {
            filter: IssueFilter::parse(query, None),
            within: None,
            ..ListQuery::default()
        };
        list(boards, &query)
            .iter()
            .map(|r| r.card.title.clone())
            .collect()
    }

    /// Does `query`, scoped to board `b`, keep the first card of `b`'s first
    /// column?
    fn keeps(b: &BoardView, query: &str) -> bool {
        let row = ListRow {
            board: b,
            card: &b.columns[0].cards[0],
            placement: Placement::Column(&b.columns[0]),
        };
        IssueFilter::parse(query, Some(&b.id)).matches(&row)
    }

    fn one(c: CardView) -> BoardView {
        board("headway", vec![c], vec![], vec![])
    }

    #[test]
    fn empty_filter_is_inactive_and_matches_all() {
        let f = IssueFilter::parse("   ", None);
        assert!(!f.is_active());
        assert!(keeps(&one(card(1, "anything", &[])), "  "));
    }

    #[test]
    fn text_terms_match_title_description_and_labels() {
        let mut c = card(1, "Fix the bar", &["ui"]);
        c.description = "wobbles on resize".to_string();
        let b = one(c);
        assert!(keeps(&b, "bar"));
        assert!(keeps(&b, "WOBBLES")); // case-insensitive
        assert!(keeps(&b, "ui")); // also searches labels
        assert!(keeps(&b, "fix wobbles")); // terms AND
        assert!(!keeps(&b, "fix nope"));
    }

    #[test]
    fn label_terms_scope_to_labels() {
        let b = one(card(1, "perf work", &["bug", "headway"]));
        assert!(keeps(&b, "label:bug"));
        assert!(keeps(&b, "l:head")); // short prefix, substring
        assert!(!keeps(&b, "label:perf")); // in the title, not a label
        assert!(keeps(&b, "label:bug perf"));
        // A bare `label:` is no constraint.
        assert!(!IssueFilter::parse("label:", None).is_active());
    }

    /// Card 0's id encodes to `abandon-abandon-abandon`.
    #[test]
    fn references_match_by_id_words() {
        let b = one(card(0, "Fix the bar", &[]));
        assert!(keeps(&b, "abandon-abandon-abandon"));
        assert!(keeps(&b, "headway:headway/abandon-abandon-abandon"));
        assert!(keeps(&b, "HEADWAY/abandon-abandon-abandon"));
        assert!(!keeps(&b, "headway/zoo-zoo-zoo"));
        // Another board's reference is plain text in a board-scoped filter.
        let mut citing = card(0, "dup", &[]);
        citing.description = "see other/abandon-abandon-abandon".to_string();
        assert!(keeps(&one(citing), "other/abandon-abandon-abandon"));
        assert!(!keeps(&b, "other/abandon-abandon-abandon"));
    }

    /// Unscoped, a reference names one card on one board.
    #[test]
    fn unscoped_reference_names_its_board() {
        let boards = [
            board("one", vec![card(0, "on one", &[])], vec![], vec![]),
            board("two", vec![card(0, "on two", &[])], vec![], vec![]),
        ];
        assert_eq!(
            titles(&boards, "headway:two/abandon-abandon-abandon"),
            ["on two"]
        );
        assert_eq!(titles(&boards, "abandon"), ["on one", "on two"]);
    }

    #[test]
    fn priority_terms_are_any_of() {
        let mut urgent = card(1, "urgent", &[]);
        urgent.priority = Priority::Urgent;
        let mut high = card(2, "high", &[]);
        high.priority = Priority::High;
        let none = card(3, "none", &[]);
        let boards = [board("b", vec![urgent, high, none], vec![], vec![])];
        assert_eq!(titles(&boards, "p:high"), ["high"]);
        assert_eq!(titles(&boards, "priority:high,urgent"), ["urgent", "high"]);
        assert_eq!(titles(&boards, "p:none"), ["none"]);
        // An unknown value is no constraint, not "no priority".
        assert_eq!(titles(&boards, "p:soon").len(), 3);
    }

    #[test]
    fn state_terms_follow_columns_and_rollups() {
        let mut parent = card(1, "parent", &[]);
        parent.subissues = vec![SubissueView {
            id: NoteId::new([4; 32]),
            title: "child".to_string(),
            column: Some("done".to_string()),
            done: true,
            archived: false,
            seq: None,
        }];
        let boards = [board(
            "b",
            vec![card(2, "todo", &[]), parent],
            vec![card(3, "doing", &[])],
            vec![card(4, "child", &[])],
        )];
        assert_eq!(titles(&boards, "is:open"), ["todo", "doing"]);
        // A parent whose subissues are all done is done wherever it sits.
        assert_eq!(titles(&boards, "is:done"), ["parent", "child"]);
        assert_eq!(titles(&boards, "is:progress"), ["doing"]);
        assert_eq!(titles(&boards, "is:any").len(), 4);
        assert_eq!(titles(&boards, "col:todo"), ["todo", "parent"]);
        assert_eq!(
            titles(&boards, "status:in-progress,DONE"),
            ["doing", "child"]
        );
    }

    /// With no column flagged terminal, the last column is done.
    #[test]
    fn done_falls_back_to_the_last_column() {
        let mut b = board(
            "b",
            vec![card(1, "a", &[])],
            vec![],
            vec![card(2, "z", &[])],
        );
        b.columns[2].terminal = false;
        assert_eq!(titles(&[b], "is:done"), ["z"]);
    }

    #[test]
    fn blocked_and_due_terms() {
        let mut blocked = card(1, "blocked", &[]);
        blocked.blocked_by = vec![EdgeRef {
            id: NoteId::new([9; 32]),
            title: String::new(),
            done: false,
        }];
        let mut cleared = card(2, "cleared", &[]);
        cleared.blocked_by = vec![EdgeRef {
            id: NoteId::new([9; 32]),
            title: String::new(),
            done: true,
        }];
        cleared.due = Date::parse("2026-10-01");
        let mut late = card(3, "late", &[]);
        late.due = Date::parse("2026-12-01");
        let boards = [board("b", vec![blocked, cleared, late], vec![], vec![])];
        assert_eq!(titles(&boards, "is:blocked"), ["blocked"]);
        assert_eq!(titles(&boards, "is:unblocked"), ["cleared", "late"]);
        assert_eq!(titles(&boards, "due:any"), ["cleared", "late"]);
        assert_eq!(titles(&boards, "due:none"), ["blocked"]);
        assert_eq!(titles(&boards, "due:2026-10-31"), ["cleared"]);
        assert_eq!(titles(&boards, "due:2026-10-01"), ["cleared"]);
    }

    #[test]
    fn hide_subissues_drops_children() {
        let mut child = card(2, "child", &[]);
        child.parent = Some(NoteId::new([1; 32]));
        let boards = [board("b", vec![card(1, "top", &[]), child], vec![], vec![])];
        let filter = IssueFilter {
            hide_subissues: true,
            ..IssueFilter::default()
        };
        assert!(filter.is_active());
        let query = ListQuery {
            filter,
            within: None,
            ..ListQuery::default()
        };
        let kept: Vec<&str> = list(&boards, &query)
            .iter()
            .map(|r| r.card.title.as_str())
            .collect();
        assert_eq!(kept, ["top"]);
    }

    /// `list_positions` is the walk as indices into the boards handed in: a
    /// board scope skips the other boards but keeps their indices, archived
    /// cards come after a board's live ones, and the order is the sorting's.
    #[test]
    fn list_positions_index_the_boards_handed_in() {
        let mut urgent = card(5, "urgent", &["ui"]);
        urgent.priority = Priority::Urgent;
        let mut shelved = card(6, "shelved", &["ui"]);
        shelved.priority = Priority::High;
        let mut one = board("one", vec![card(1, "a", &["ui"])], vec![], vec![]);
        one.archived = vec![ArchivedCard {
            card: card(2, "one-shelved", &["ui"]),
            from: None,
        }];
        let mut two = board(
            "two",
            vec![card(3, "low", &["ui"]), card(4, "plain", &[])],
            vec![urgent],
            vec![],
        );
        two.archived = vec![ArchivedCard {
            card: shelved,
            from: None,
        }];
        let boards = [one, two];

        let scoped = ListQuery {
            filter: IssueFilter::parse("label:ui", None),
            board: Some("two".to_string()),
            sorting: Sorting::by(SortBy::Priority),
            ..ListQuery::default()
        };
        let pos = |board, slot| RowPos { board, slot };
        assert_eq!(
            list_positions(&boards, &scoped),
            [
                pos(1, RowSlot::Column { col: 1, card: 0 }),
                pos(1, RowSlot::Column { col: 0, card: 0 }),
            ]
        );

        let archived = ListQuery {
            filter: IssueFilter::parse("is:archived", None),
            sorting: Sorting::by(SortBy::Priority),
            ..ListQuery::default()
        };
        let positions = list_positions(&boards, &archived);
        assert_eq!(
            positions,
            [pos(1, RowSlot::Archived(0)), pos(0, RowSlot::Archived(0))]
        );
        // Each position reads back as the row `list` hands out for it.
        let rows = list(&boards, &archived);
        for (pos, row) in positions.iter().zip(&rows) {
            let back = pos.row(&boards[pos.board]);
            assert!(std::ptr::eq(back.card, row.card));
            assert!(std::ptr::eq(back.board, row.board));
            assert!(back.is_archived());
        }
        assert_eq!(rows[0].card.title, "shelved");
        assert_eq!(rows[1].card.title, "one-shelved");
    }

    /// Rows come board by board, column by column, and archived cards never
    /// show.
    #[test]
    fn list_keeps_board_order_and_skips_archived() {
        let mut first = board(
            "one",
            vec![card(1, "a", &[])],
            vec![],
            vec![card(2, "b", &[])],
        );
        first.archived = vec![ArchivedCard {
            card: card(3, "gone", &[]),
            from: None,
        }];
        let second = board("two", vec![card(4, "c", &[])], vec![], vec![]);
        let boards = [first, second];
        assert_eq!(titles(&boards, ""), ["a", "b", "c"]);
        let rows = list(&boards, &ListQuery::default());
        assert_eq!(rows[2].board.id, "two");
        assert_eq!(rows[1].place_name(), "Done");
    }

    /// `is:archived` lists the archived cards and nothing else; `is:done` and
    /// `is:any` stay live-only.
    #[test]
    fn is_archived_lists_only_archived_cards() {
        let mut b = board(
            "b",
            vec![card(1, "live", &["ui"])],
            vec![],
            vec![card(2, "done", &[])],
        );
        b.archived = vec![
            ArchivedCard {
                card: card(3, "shelved", &["ui"]),
                from: Some("done".to_string()),
            },
            ArchivedCard {
                card: card(4, "dropped", &[]),
                from: None,
            },
        ];
        let boards = [b];
        assert_eq!(titles(&boards, "is:archived"), ["shelved", "dropped"]);
        assert_eq!(titles(&boards, "is:archived label:ui"), ["shelved"]);
        assert_eq!(titles(&boards, "is:done"), ["done"]);
        assert_eq!(titles(&boards, "is:any"), ["live", "done"]);
        assert_eq!(titles(&boards, "is:open"), ["live"]);
        // A column term never matches an archived card.
        assert!(titles(&boards, "is:archived col:done").is_empty());
        let rows = list(
            &boards,
            &ListQuery {
                filter: IssueFilter::parse("is:archived", None),
                within: None,
                ..ListQuery::default()
            },
        );
        assert!(rows.iter().all(ListRow::is_archived));
        assert_eq!(rows[0].place_name(), "archived");
        assert!(!rows[0].is_done());
    }

    /// `within` keeps the container and everything under it, archived
    /// descendants included in the set.
    #[test]
    fn within_keeps_the_subtree() {
        let sub = |n: u8| SubissueView {
            id: NoteId::new([n; 32]),
            title: String::new(),
            column: None,
            done: false,
            archived: false,
            seq: None,
        };
        let mut epic = card(1, "epic", &[]);
        epic.subissues = vec![sub(2)];
        let mut child = card(2, "child", &[]);
        child.subissues = vec![sub(3)];
        let mut b = board(
            "b",
            vec![epic, child, card(4, "other", &[])],
            vec![],
            vec![],
        );
        b.archived = vec![ArchivedCard {
            card: card(3, "grandchild", &[]),
            from: None,
        }];
        assert_eq!(
            subtree_ids(&b, [1; 32]),
            HashSet::from([[1; 32], [2; 32], [3; 32]])
        );
        let boards = [b];
        let query = ListQuery {
            filter: IssueFilter::default(),
            within: Some([1; 32]),
            ..ListQuery::default()
        };
        let kept: Vec<&str> = list(&boards, &query)
            .iter()
            .map(|r| r.card.title.as_str())
            .collect();
        assert_eq!(kept, ["epic", "child"]);
    }

    /// Names parse ignoring case, with the aliases people write; anything
    /// else is an error naming the choices.
    #[test]
    fn display_props_parse_by_name() {
        assert_eq!("labels".parse(), Ok(DisplayProp::Labels));
        assert_eq!(" Due ".parse(), Ok(DisplayProp::Due));
        assert_eq!("sub-issues".parse(), Ok(DisplayProp::SubIssues));
        assert_eq!("blocked".parse(), Ok(DisplayProp::Blockers));
        for prop in DisplayProp::ALL {
            assert_eq!(prop.name().parse(), Ok(prop), "{prop} round-trips");
        }
        let err = "assignee".parse::<DisplayProp>().unwrap_err();
        assert!(
            err.to_string().contains("expected one of priority, id"),
            "{err}"
        );

        let set = DisplayProps::parse_list("labels, due,,").unwrap();
        assert_eq!(
            set,
            DisplayProps::of(&[DisplayProp::Labels, DisplayProp::Due])
        );
        assert_eq!(DisplayProps::parse_list(""), Ok(DisplayProps::NONE));
        assert!(DisplayProps::parse_list("labels,nope").is_err());
    }

    /// The default is Linear's set, `--show` replaces it and `--hide` takes
    /// from whichever applies.
    #[test]
    fn display_props_default_show_and_hide() {
        let default = DisplayProps::default();
        assert!(default.contains(DisplayProp::Labels));
        assert!(default.contains(DisplayProp::Updated));
        assert!(!default.contains(DisplayProp::Created));
        assert!(!default.contains(DisplayProp::Board));

        let hidden = DisplayProps::shown_hidden(None, DisplayProps::of(&[DisplayProp::Labels]));
        assert!(!hidden.contains(DisplayProp::Labels));
        assert!(hidden.contains(DisplayProp::Status));

        let only = DisplayProps::of(&[DisplayProp::Id, DisplayProp::Board]);
        let shown = DisplayProps::shown_hidden(Some(only), DisplayProps::of(&[DisplayProp::Id]));
        assert_eq!(shown.iter().collect::<Vec<_>>(), [DisplayProp::Board]);

        let mut set = DisplayProps::NONE;
        set.toggle(DisplayProp::Due);
        assert!(set.contains(DisplayProp::Due));
        set.toggle(DisplayProp::Due);
        assert_eq!(set, DisplayProps::NONE);
    }

    /// A set serializes as its names in row order, so a saved view reads
    /// plainly and parses back.
    #[test]
    fn display_props_serialize_as_names() {
        let set = DisplayProps::of(&[DisplayProp::Due, DisplayProp::Priority]);
        let json = serde_json::to_string(&set).unwrap();
        assert_eq!(json, r#"["priority","due"]"#);
        assert_eq!(serde_json::from_str::<DisplayProps>(&json).unwrap(), set);
    }

    /// Each of `groups` as `"<header>: <title> <title>…"`, for comparing.
    fn group_titles(rows: &[ListRow], groups: &[ListGroup]) -> Vec<String> {
        groups
            .iter()
            .map(|g| {
                let titles: Vec<&str> = g
                    .rows
                    .iter()
                    .map(|&at| rows[at].card.title.as_str())
                    .collect();
                format!("{}: {}", g.label, titles.join(" "))
            })
            .collect()
    }

    /// Two boards whose columns share names: one board's "Todo" and the
    /// other's are one status group, and the terminal "Done" goes last even
    /// though the second board lists it before "Doing".
    #[test]
    fn group_by_status_merges_columns_by_name() {
        let one = board(
            "one",
            vec![card(1, "a", &[])],
            vec![],
            vec![card(2, "b", &[])],
        );
        let mut two = board("two", vec![card(3, "c", &[])], vec![], vec![]);
        two.columns.insert(
            1,
            column("review", "Review", false, vec![card(4, "d", &[])]),
        );
        let boards = [one, two];
        let refs: Vec<&BoardView> = boards.iter().collect();
        let rows = list(&boards, &ListQuery::default());
        let by = |show_empty| Grouping {
            by: GroupBy::Status,
            show_empty,
            ..Grouping::default()
        };
        let groups = group(&refs, &rows, by(false));
        assert_eq!(
            group_titles(&rows, &groups),
            ["Todo: a c", "Review: d", "Done: b",]
        );
        assert_eq!(groups[0].key, GroupKey::Status("todo".to_string()));
        // Empty columns show only when asked, still in column order.
        let labels: Vec<String> = group(&refs, &rows, by(true))
            .into_iter()
            .map(|g| g.label)
            .collect();
        assert_eq!(labels, ["Todo", "In Progress", "Review", "Done"]);
    }

    #[test]
    fn group_by_priority_runs_urgent_to_none() {
        let mut low = card(1, "low", &[]);
        low.priority = Priority::Low;
        let mut urgent = card(2, "urgent", &[]);
        urgent.priority = Priority::Urgent;
        let boards = [board(
            "b",
            vec![low, card(3, "none", &[]), urgent],
            vec![],
            vec![],
        )];
        let refs: Vec<&BoardView> = boards.iter().collect();
        let rows = list(&boards, &ListQuery::default());
        let mut grouping = Grouping {
            by: GroupBy::Priority,
            ..Grouping::default()
        };
        let groups = group(&refs, &rows, grouping);
        assert_eq!(
            group_titles(&rows, &groups),
            ["Urgent: urgent", "Low: low", "No priority: none",]
        );
        grouping.show_empty = true;
        assert_eq!(group(&refs, &rows, grouping).len(), 5);
    }

    /// A card with two labels is in both groups; unlabelled cards gather
    /// under "No label", last. Label groups have no empties to show.
    #[test]
    fn group_by_label_fans_out() {
        let boards = [board(
            "b",
            vec![
                card(1, "both", &["UI", "bug"]),
                card(2, "plain", &[]),
                card(3, "ui only", &["ui"]),
            ],
            vec![],
            vec![],
        )];
        let refs: Vec<&BoardView> = boards.iter().collect();
        let rows = list(&boards, &ListQuery::default());
        let grouping = Grouping {
            by: GroupBy::Label,
            show_empty: true,
            ..Grouping::default()
        };
        let groups = group(&refs, &rows, grouping);
        assert_eq!(
            group_titles(&rows, &groups),
            ["bug: both", "UI: both ui only", "No label: plain",]
        );
        assert_eq!(groups[2].key, GroupKey::Label(None));
    }

    #[test]
    fn group_by_board_and_parent() {
        let mut child = card(2, "child", &[]);
        child.parent = Some(NoteId::new([1; 32]));
        let boards = [
            board("one", vec![card(1, "epic", &[]), child], vec![], vec![]),
            board("two", vec![], vec![], vec![]),
        ];
        let refs: Vec<&BoardView> = boards.iter().collect();
        let rows = list(&boards, &ListQuery::default());
        let by = |by, show_empty| Grouping {
            by,
            show_empty,
            ..Grouping::default()
        };
        let boards_only: Vec<String> = group(&refs, &rows, by(GroupBy::Board, false))
            .into_iter()
            .map(|g| g.label)
            .collect();
        assert_eq!(boards_only, ["one"]);
        assert_eq!(group(&refs, &rows, by(GroupBy::Board, true)).len(), 2);

        let parents = group(&refs, &rows, by(GroupBy::Parent, false));
        assert_eq!(
            group_titles(&rows, &parents),
            ["epic: child", "No parent: epic",]
        );
        assert_eq!(parents[0].key, GroupKey::Parent(Some(NoteId::new([1; 32]))));
    }

    /// Sub-groups split each group again; the same grouping twice, or none
    /// on top, has no sub-groups. Ungrouped is one group of every row.
    #[test]
    fn sub_grouping_splits_each_group() {
        let mut high = card(1, "high", &[]);
        high.priority = Priority::High;
        let boards = [board(
            "b",
            vec![high, card(2, "none", &[])],
            vec![],
            vec![card(3, "done", &[])],
        )];
        let refs: Vec<&BoardView> = boards.iter().collect();
        let rows = list(&boards, &ListQuery::default());
        let grouping = Grouping {
            by: GroupBy::Status,
            sub: GroupBy::Priority,
            show_empty: false,
        };
        let groups = group(&refs, &rows, grouping);
        assert_eq!(groups[0].rows.len(), 2);
        assert_eq!(
            group_titles(&rows, &groups[0].subgroups),
            ["High: high", "No priority: none",]
        );
        assert_eq!(groups[1].subgroups.len(), 1);

        let same = Grouping {
            sub: GroupBy::Status,
            ..grouping
        };
        assert!(group(&refs, &rows, same)[0].subgroups.is_empty());
        let flat = group(&refs, &rows, Grouping::default());
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].key, GroupKey::All);
        assert_eq!(flat[0].rows, [0, 1, 2]);
        assert!(flat[0].subgroups.is_empty());
    }

    /// Names parse with their aliases; a grouped row drops the property its
    /// header already says.
    #[test]
    fn group_by_parses_and_implies_props() {
        assert_eq!("Labels".parse(), Ok(GroupBy::Label));
        assert_eq!("column".parse(), Ok(GroupBy::Status));
        assert_eq!("epic".parse(), Ok(GroupBy::Parent));
        for by in GroupBy::ALL {
            assert_eq!(by.name().parse(), Ok(by), "{by} round-trips");
        }
        let err = "assignee".parse::<GroupBy>().unwrap_err();
        assert!(
            err.to_string().contains("expected one of none, status"),
            "{err}"
        );

        let grouping = Grouping {
            by: GroupBy::Status,
            sub: GroupBy::Label,
            show_empty: false,
        };
        let shown = grouping.row_display(DisplayProps::default());
        assert!(!shown.contains(DisplayProp::Status));
        assert!(!shown.contains(DisplayProp::Labels));
        assert!(shown.contains(DisplayProp::Priority));
        assert_eq!(
            Grouping::default().row_display(DisplayProps::default()),
            DisplayProps::default()
        );
        let json = serde_json::to_string(&grouping).unwrap();
        assert_eq!(json, r#"{"by":"status","sub":"label","show_empty":false}"#);
        assert_eq!(serde_json::from_str::<Grouping>(&json).unwrap(), grouping);
    }

    /// The titles of every live card on `boards` (`is:any`), in `sorting`'s
    /// order.
    fn sorted(boards: &[BoardView], sorting: Sorting) -> Vec<String> {
        let query = ListQuery {
            filter: IssueFilter::parse("is:any", None),
            sorting,
            ..ListQuery::default()
        };
        list(boards, &query)
            .iter()
            .map(|r| r.card.title.clone())
            .collect()
    }

    /// Priority runs Urgent first by default and Low first ascending, cards
    /// with none last both ways; equal priorities go most recently updated
    /// first.
    #[test]
    fn sort_by_priority_keeps_none_last() {
        let with = |n, title, p, updated| {
            let mut c = card(n, title, &[]);
            c.priority = p;
            c.updated_at = updated;
            c
        };
        let boards = [board(
            "b",
            vec![
                with(1, "none", Priority::None, 9),
                with(2, "low", Priority::Low, 0),
                with(3, "old high", Priority::High, 1),
                with(4, "urgent", Priority::Urgent, 0),
                with(5, "new high", Priority::High, 5),
            ],
            vec![],
            vec![],
        )];
        let by = Sorting::by(SortBy::Priority);
        assert!(by.descending);
        assert_eq!(
            sorted(&boards, by),
            ["urgent", "new high", "old high", "low", "none"]
        );
        let ascending = Sorting {
            descending: false,
            ..by
        };
        assert_eq!(
            sorted(&boards, ascending),
            ["low", "new high", "old high", "urgent", "none"]
        );
    }

    /// Due runs soonest first, undated cards last both ways; created and
    /// updated run newest first; manual is board order, reversed descending.
    #[test]
    fn sort_by_dates_and_manual() {
        let with = |n, title, due: Option<&str>, at| {
            let mut c = card(n, title, &[]);
            c.due = due.and_then(Date::parse);
            c.created_at = at;
            c.updated_at = 10 - at;
            c
        };
        let boards = [board(
            "b",
            vec![
                with(1, "undated", None, 1),
                with(2, "late", Some("2026-12-01"), 3),
            ],
            vec![with(3, "soon", Some("2026-10-20"), 2)],
            vec![],
        )];
        let due = Sorting::by(SortBy::Due);
        assert_eq!(sorted(&boards, due), ["soon", "late", "undated"]);
        let due_desc = Sorting {
            descending: true,
            ..due
        };
        assert_eq!(sorted(&boards, due_desc), ["late", "soon", "undated"]);
        assert_eq!(
            sorted(&boards, Sorting::by(SortBy::Created)),
            ["late", "soon", "undated"]
        );
        assert_eq!(
            sorted(&boards, Sorting::by(SortBy::Updated)),
            ["undated", "soon", "late"]
        );
        assert_eq!(
            sorted(&boards, Sorting::default()),
            ["undated", "late", "soon"]
        );
        let reversed = Sorting {
            descending: true,
            ..Sorting::default()
        };
        assert_eq!(sorted(&boards, reversed), ["soon", "late", "undated"]);
    }

    /// Completed by recency puts done rows after the open ones, most
    /// recently updated first, whatever the main ordering says.
    #[test]
    fn completed_by_recency_trails_done_rows() {
        let with = |n, title, p, updated| {
            let mut c = card(n, title, &[]);
            c.priority = p;
            c.updated_at = updated;
            c
        };
        let boards = [board(
            "b",
            vec![with(1, "open low", Priority::Low, 0)],
            vec![],
            vec![
                with(2, "done old urgent", Priority::Urgent, 1),
                with(3, "done new", Priority::None, 7),
            ],
        )];
        let by = Sorting::by(SortBy::Priority);
        assert_eq!(
            sorted(&boards, by),
            ["done old urgent", "open low", "done new"]
        );
        let recency = Sorting {
            completed_by_recency: true,
            ..by
        };
        assert_eq!(
            sorted(&boards, recency),
            ["open low", "done new", "done old urgent"]
        );
    }

    /// Work order is each board's `traversal::work_order` walk, board by
    /// board, and the ready set is what `traversal::ready` hands out.
    #[test]
    fn sort_by_work_order_matches_traversal() {
        use crate::event::Container;
        let sub = |n: u8| SubissueView {
            id: NoteId::new([n; 32]),
            title: String::new(),
            column: Some("todo".to_string()),
            done: false,
            archived: false,
            seq: None,
        };
        let mut first = card(1, "made first", &[]);
        first.created_at = 1;
        let mut epic = card(2, "epic, sequenced", &[]);
        epic.seq = Some("a".to_string());
        epic.created_at = 2;
        epic.subissues = vec![sub(3)];
        let mut child = card(3, "child", &[]);
        child.parent = Some(NoteId::new([2; 32]));
        child.created_at = 3;
        let mut doing = card(4, "doing", &[]);
        doing.created_at = 0;
        let boards = [
            board("one", vec![first, epic, child], vec![doing], vec![]),
            board("two", vec![card(5, "elsewhere", &[])], vec![], vec![]),
        ];
        let walk: Vec<String> =
            crate::traversal::work_order(&boards[0], &Container::BoardRoot("one".to_string()))
                .iter()
                .map(|c| c.title.clone())
                .collect();
        assert_eq!(walk, ["epic, sequenced", "child", "doing", "made first"]);
        let mut expected = walk.clone();
        expected.push("elsewhere".to_string());
        assert_eq!(sorted(&boards, Sorting::by(SortBy::WorkOrder)), expected);

        let rows = list(&boards, &ListQuery::default());
        let ready = ready_set(&rows);
        let mut frontier: Vec<[u8; 32]> =
            crate::traversal::ready(&boards[0], &Container::BoardRoot("one".to_string()))
                .iter()
                .map(|c| *c.id.bytes())
                .collect();
        frontier.push([5; 32]);
        assert_eq!(ready, frontier.into_iter().collect::<HashSet<_>>());
        // The epic waits on its child, and In Progress is taken.
        assert!(!ready.contains(&[2; 32]));
        assert!(!ready.contains(&[4; 32]));
        assert!(ready.contains(&[3; 32]));
    }

    #[test]
    fn sort_by_parses_names_and_aliases() {
        assert_eq!("work-order".parse(), Ok(SortBy::WorkOrder));
        assert_eq!("seq".parse(), Ok(SortBy::WorkOrder));
        assert_eq!("Board".parse(), Ok(SortBy::Manual));
        for by in SortBy::ALL {
            assert_eq!(by.name().parse(), Ok(by), "{by} round-trips");
        }
        let err = "assignee".parse::<SortBy>().unwrap_err();
        assert!(
            err.to_string().contains("expected one of manual, priority"),
            "{err}"
        );
        let sorting = Sorting {
            completed_by_recency: true,
            ..Sorting::by(SortBy::WorkOrder)
        };
        let json = serde_json::to_string(&sorting).unwrap();
        assert_eq!(
            json,
            r#"{"by":"work-order","descending":false,"completed_by_recency":true}"#
        );
        assert_eq!(serde_json::from_str::<Sorting>(&json).unwrap(), sorting);
    }
}
