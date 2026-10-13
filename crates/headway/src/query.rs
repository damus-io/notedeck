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
//! - [`ListQuery`] + [`list`] — flatten boards into the rows a query keeps.

use std::collections::{HashMap, HashSet};

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

/// What a list shows: the [`IssueFilter`] and where to look.
///
/// Grouping and ordering land here as the list view grows
/// (headway:headway/acoustic-canvas-diesel).
#[derive(Clone, Debug, Default)]
pub struct ListQuery {
    /// Which cards to keep.
    pub filter: IssueFilter,
    /// Only this card and its descendants (`--in <card>`), on whichever board
    /// holds it. `None` looks at every card.
    pub within: Option<[u8; 32]>,
    /// Which properties each row shows beside its title. Rendering only:
    /// [`list`] keeps the same rows whatever it holds.
    pub display: DisplayProps,
}

/// Every card on `boards` that `query` keeps, in board order: board by board,
/// column by column, each column by rank — the board as drawn. Archived cards
/// are listed only when the filter asks for them ([`IssueFilter::wants_archived`]),
/// after each board's live ones, so a default list never changes for them.
///
/// Sorting and grouping are later cards of the list-view epic; until then the
/// order is the boards' own.
pub fn list<'a>(boards: &'a [BoardView], query: &ListQuery) -> Vec<ListRow<'a>> {
    let mut rows = Vec::new();
    for board in boards {
        let within = query.within.map(|root| subtree_ids(board, root));
        let mut keep = |row: ListRow<'a>| {
            let inside = within
                .as_ref()
                .is_none_or(|ids| ids.contains(row.card.id.bytes()));
            if inside && query.filter.matches(&row) {
                rows.push(row);
            }
        };
        for column in &board.columns {
            for card in &column.cards {
                keep(ListRow {
                    board,
                    card,
                    placement: Placement::Column(column),
                });
            }
        }
        if !query.filter.wants_archived() {
            continue;
        }
        for archived in &board.archived {
            keep(ListRow {
                board,
                card: &archived.card,
                placement: Placement::Archived,
            });
        }
    }
    rows
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
}
