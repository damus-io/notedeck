//! The value types a board and its cards are described in: scalar [`Field`]s
//! and their typed values ([`Priority`], [`Date`]), the column definitions and
//! the terminal-column rule, and the [`BoardCoord`] owner+slug identity.

use nostrdb_net::Pubkey;

use super::kinds::KIND_BOARD;

/// A single-value scalar overlay on a card. Each [`Field`] is carried as a
/// kind-1985 NIP-32 label in its own `L` namespace with one `l` value, resolved
/// latest-authorised-wins exactly like the subject overlay ([`build_field`](super::build_field),
/// [`FieldEdit`](super::FieldEdit)). Publishing an empty (or, for priority, `"none"`) value clears
/// the field.
///
/// This is deliberately only for *single scalar* fields — multi-valued concerns
/// (labels, a set) and entity references (a parent relation, a board placement)
/// keep their own mechanisms rather than being forced through here. The plumbing
/// (builder, parse, reducer overlay, activity row) is generic over the field;
/// each field's *value type* and rendering stay typed at the edges, landing in a
/// typed [`CardView`](super::CardView) field (e.g. [`CardView::priority`](super::CardView::priority), [`CardView::due`](super::CardView::due),
/// [`CardView::estimate`](super::CardView::estimate)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Field {
    Priority,
    Due,
    Estimate,
}

impl Field {
    /// The NIP-32 `L` namespace that carries this field on a kind-1985 label.
    pub(super) fn namespace(self) -> &'static str {
        match self {
            Field::Priority => "#priority",
            Field::Due => "#due",
            Field::Estimate => "#estimate",
        }
    }

    /// The field carried by an `L` namespace, or `None` if it isn't a scalar
    /// field namespace (e.g. `#subject`/`#t`, which are handled separately).
    pub(super) fn from_namespace(ns: &str) -> Option<Field> {
        match ns {
            "#priority" => Some(Field::Priority),
            "#due" => Some(Field::Due),
            "#estimate" => Some(Field::Estimate),
            _ => None,
        }
    }

    /// A human label for the field, used in the activity timeline and JSON.
    pub fn label(self) -> &'static str {
        match self {
            Field::Priority => "priority",
            Field::Due => "due",
            Field::Estimate => "estimate",
        }
    }
}

/// A card's priority. Ordered least-to-most urgent so a "sort by priority"
/// descends from [`Priority::Urgent`]; [`Priority::None`] (the default, "no
/// priority") sorts last, matching Linear. Carried as the [`Field::Priority`]
/// scalar overlay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// No priority set — the default when a card has never been prioritised.
    #[default]
    None,
    Low,
    Medium,
    High,
    Urgent,
}

impl Priority {
    /// The stable wire/JSON string for this priority.
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::None => "none",
            Priority::Low => "low",
            Priority::Medium => "medium",
            Priority::High => "high",
            Priority::Urgent => "urgent",
        }
    }

    /// Parse a priority from its wire string (case-insensitive). `"med"` is
    /// accepted as an alias for `"medium"`. Unknown values (and `"none"`) map to
    /// [`Priority::None`], so a malformed overlay reads as "no priority" rather
    /// than failing the fold.
    pub fn parse(s: &str) -> Priority {
        match s.trim().to_ascii_lowercase().as_str() {
            "urgent" => Priority::Urgent,
            "high" => Priority::High,
            "medium" | "med" => Priority::Medium,
            "low" => Priority::Low,
            _ => Priority::None,
        }
    }
}

/// A calendar day — the value type of the [`Field::Due`] due-date overlay. Day
/// granularity (not an instant): a due date is "the 30th", independent of
/// timezone. Fields are ordered year→month→day so the derived `Ord` is
/// chronological, which is exactly the sort the list view wants. Rendered and
/// parsed as ISO `YYYY-MM-DD`, which also happens to sort lexicographically the
/// same way, so the wire form sorts correctly too.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl Date {
    /// Parse an ISO `YYYY-MM-DD` date, validating the month and the day against
    /// that month's length (leap years included). `None` for anything malformed
    /// or out of range, so a junk overlay reads as "no due date".
    pub fn parse(s: &str) -> Option<Date> {
        let (y, rest) = s.trim().split_once('-')?;
        let (m, d) = rest.split_once('-')?;
        let year: i32 = y.parse().ok()?;
        let month: u8 = m.parse().ok()?;
        let day: u8 = d.parse().ok()?;
        if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
            return None;
        }
        Some(Date { year, month, day })
    }
}

impl std::fmt::Display for Date {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// Days in `month` of `year` (1-indexed month), honouring leap years for
/// February. Used to validate [`Date::parse`].
fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Sentinel placement column id meaning the card has been removed from the
/// board. A card whose latest *authorised* placement points here is dropped by
/// the reducer. This is a reversible "tombstone" (re-place the card to restore
/// it) rather than a NIP-09 deletion, which keeps removal under the same
/// authority/latest-wins rules as every other placement.
pub const COL_DELETED: &str = "__deleted__";

/// Sentinel placement column id meaning the card has been *archived*: taken off
/// the active board but kept (and recoverable) rather than tombstoned. A card
/// whose latest *authorised* placement points here is collected onto
/// [`BoardView::archived`](super::BoardView::archived) instead of a column. The archive placement also
/// carries a `from` tag (the column it was archived from) so a restore lands the
/// card back where it was — see [`build_archive_placement`](super::build_archive_placement). Like `COL_DELETED`
/// this keeps archival under the same authority/latest-wins rules as any
/// placement.
pub const COL_ARCHIVED: &str = "__archived__";

/// A column definition as carried on the board event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnDef {
    pub id: String,
    pub name: String,
    /// A *terminal* column is a "done" column: a card sitting here counts as
    /// finished — it clears its dependents, drops out of the ready frontier, and
    /// renders as done. A board may mark several (e.g. both `In Review` and
    /// `Done`). When a board marks *none* — every board authored before this flag
    /// existed — its last column is treated as terminal, preserving the original
    /// positional behaviour. See [`column_is_terminal`].
    pub terminal: bool,
}

impl ColumnDef {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            terminal: false,
        }
    }

    /// Mark this column terminal (a "done" column). Builder sugar for the
    /// default-board definition and column-editing call sites.
    pub fn terminal(mut self) -> Self {
        self.terminal = true;
        self
    }
}

/// Whether the column `col_id` is *terminal* on a board whose columns are given,
/// in order, as `(id, terminal)` pairs.
///
/// A terminal column is one where a card counts as done: it clears its
/// dependents, leaves the [`crate::traversal`] ready frontier, and renders as
/// done. Terminal columns are marked explicitly on the board definition
/// ([`ColumnDef::terminal`]). A board that marks *none* — every board created
/// before the flag existed — falls back to treating its **last** column as
/// terminal, which is exactly the original positional `columns.last()` rule and
/// so needs no migration.
///
/// Single pass, no allocation: safe to call from per-frame render paths.
pub fn column_is_terminal<'a>(
    columns: impl IntoIterator<Item = (&'a str, bool)>,
    col_id: &str,
) -> bool {
    let mut any_marked = false;
    let mut target_marked = false;
    let mut last_is_target = false;
    for (id, terminal) in columns {
        if terminal {
            any_marked = true;
            target_marked |= id == col_id;
        }
        last_is_target = id == col_id;
    }
    if any_marked {
        target_marked
    } else {
        last_is_target
    }
}

/// The addressable identity of a board: its owner plus its slug — i.e. the nostr
/// coordinate `30619:<owner-hex>:<slug>`.
///
/// A board is `(owner, slug)`, never a bare slug: two owners can each have a board
/// with the same slug (your "roadmap" and a teammate's shared "roadmap"), so the
/// selection, switcher, and saved-preference layers key on this coordinate rather
/// than the slug alone. It is also the `#a`-tag value that anchors a board's cards,
/// and the key `fold_shared_board` gathers every member's events under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardCoord {
    /// The board owner's pubkey — the author of its kind-30619 definition. Raw
    /// bytes (not [`Pubkey`]) to match `BoardView::author` / `IssueEvent::board_author`
    /// and avoid conversions at the many construction sites.
    pub owner: [u8; 32],
    /// The board's slug: the `d`-tag identifier of its definition.
    pub slug: String,
}

impl BoardCoord {
    /// Construct from an owner pubkey and slug.
    pub fn new(owner: [u8; 32], slug: impl Into<String>) -> Self {
        Self {
            owner,
            slug: slug.into(),
        }
    }

    /// Render as the coordinate string `30619:<owner-hex>:<slug>`.
    pub fn coordinate(&self) -> String {
        board_address(&Pubkey::new(self.owner), &self.slug)
    }

    /// Parse a `30619:<owner-hex>:<slug>` coordinate. `None` if the kind prefix
    /// isn't [`KIND_BOARD`] or the owner segment isn't valid hex.
    pub fn parse(addr: &str) -> Option<BoardCoord> {
        let mut parts = addr.splitn(3, ':');
        let kind = parts.next()?;
        if kind != KIND_BOARD.to_string() {
            return None;
        }
        let owner_hex = parts.next()?;
        let slug = parts.next()?;
        let owner = *Pubkey::from_hex(owner_hex).ok()?.bytes();
        Some(BoardCoord::new(owner, slug))
    }
}

/// The structured review metadata one review record ([`KIND_REVIEW`](super::KIND_REVIEW))
/// carries: which commit finished a card, and where a reviewer can find it.
///
/// Every field is optional and maps 1:1 onto a tag of the same name (see
/// [`ReviewFields::tags`]); an absent field is an absent tag. It is both the
/// write input ([`build_review`](super::build_review),
/// `store::BoardAction::AddReview`) and the read output
/// ([`ReviewEvent::fields`](super::ReviewEvent::fields),
/// [`ReviewView::fields`](super::ReviewView::fields)), so the field list lives in
/// exactly one place.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReviewFields {
    /// The commit's full sha (40-hex for SHA-1 repos).
    pub commit: Option<String>,
    /// The commit subject line.
    pub title: Option<String>,
    /// The branch the commit was made on.
    pub branch: Option<String>,
    /// The hostname of the machine that recorded the commit.
    pub host: Option<String>,
    /// The repo toplevel on `host` (`git rev-parse --show-toplevel`).
    pub path: Option<String>,
    /// Repo identity: the root commit sha, the same across every clone and
    /// worktree, so any host can find its own checkout of the same repo.
    pub repo: Option<String>,
    /// The agentic session that did the work, as `agentium:<word-id>`.
    pub agentium: Option<String>,
    /// URL of the explainer page published for the work.
    pub explainer: Option<String>,
    /// An explicit fetch URL for the commit, overriding the host-derived one.
    pub remote: Option<String>,
}

impl ReviewFields {
    /// Every field paired with its wire tag name, in wire order. The one place
    /// the tag names are spelled: the builder writes from it, the JSON renders
    /// from it and [`ReviewFields::slot_mut`] parses into the same names.
    pub fn tags(&self) -> [(&'static str, Option<&str>); 9] {
        [
            ("commit", self.commit.as_deref()),
            ("title", self.title.as_deref()),
            ("branch", self.branch.as_deref()),
            ("host", self.host.as_deref()),
            ("path", self.path.as_deref()),
            ("repo", self.repo.as_deref()),
            ("agentium", self.agentium.as_deref()),
            ("explainer", self.explainer.as_deref()),
            ("remote", self.remote.as_deref()),
        ]
    }

    /// The field stored under wire tag `name`, or `None` for a tag that isn't a
    /// review field (e.g. the `e` card reference). Inverse of [`ReviewFields::tags`].
    pub(super) fn slot_mut(&mut self, name: &str) -> Option<&mut Option<String>> {
        Some(match name {
            "commit" => &mut self.commit,
            "title" => &mut self.title,
            "branch" => &mut self.branch,
            "host" => &mut self.host,
            "path" => &mut self.path,
            "repo" => &mut self.repo,
            "agentium" => &mut self.agentium,
            "explainer" => &mut self.explainer,
            "remote" => &mut self.remote,
            _ => return None,
        })
    }
}

/// The addressable coordinate of a board: `30619:<author-hex>:<board-id>`. Thin
/// formatting helper; see [`BoardCoord`] for the owner+slug identity type.
pub fn board_address(author: &Pubkey, board_id: &str) -> String {
    format!("{KIND_BOARD}:{}:{board_id}", author.hex())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostrdb_net::FullKeypair;

    #[test]
    fn board_coord_round_trips_and_rejects_other_kinds() {
        let kp = FullKeypair::generate();
        let coord = BoardCoord::new(*kp.pubkey.bytes(), "roadmap");

        // coordinate() matches the legacy board_address formatting exactly.
        assert_eq!(coord.coordinate(), board_address(&kp.pubkey, "roadmap"));

        // Round-trips back to the same owner + slug.
        let parsed = BoardCoord::parse(&coord.coordinate()).expect("parse own coordinate");
        assert_eq!(parsed, coord);

        // A slug containing ':' survives (splitn keeps the tail intact).
        let odd = BoardCoord::new(*kp.pubkey.bytes(), "a:b:c");
        assert_eq!(BoardCoord::parse(&odd.coordinate()), Some(odd));

        // Non-30619 kinds and malformed owners are rejected.
        assert!(BoardCoord::parse(&format!("30620:{}:roadmap", kp.pubkey.hex())).is_none());
        assert!(BoardCoord::parse("30619:not-hex:roadmap").is_none());
        assert!(BoardCoord::parse("roadmap").is_none());
    }

    #[test]
    fn priority_parses_and_orders() {
        assert_eq!(Priority::parse("Urgent"), Priority::Urgent);
        assert_eq!(Priority::parse(" high "), Priority::High);
        assert_eq!(Priority::parse("med"), Priority::Medium);
        assert_eq!(Priority::parse("none"), Priority::None);
        assert_eq!(Priority::parse("nonsense"), Priority::None);
        // "no priority" sorts below every real priority (Linear ordering).
        assert!(Priority::None < Priority::Low);
        assert!(Priority::Low < Priority::Urgent);
        assert_eq!(Priority::High.as_str(), "high");
    }

    #[test]
    fn date_parses_and_orders() {
        assert_eq!(
            Date::parse("2026-07-30"),
            Some(Date {
                year: 2026,
                month: 7,
                day: 30
            })
        );
        assert_eq!(Date::parse("2024-02-29").map(|d| d.day), Some(29)); // leap
        assert_eq!(Date::parse("2026-02-29"), None); // not a leap year
        assert_eq!(Date::parse("2026-13-01"), None); // bad month
        assert_eq!(Date::parse("nonsense"), None);
        assert!(Date::parse("2026-01-31") < Date::parse("2026-02-01"));
        assert_eq!(Date::parse("2026-07-30").unwrap().to_string(), "2026-07-30");
    }

    /// The terminal predicate: an explicitly-marked column wins, and a board with
    /// no marks falls back to its last column (the pre-flag positional rule).
    #[test]
    fn column_is_terminal_marks_and_fallback() {
        // No column marked → only the last column is terminal.
        let unmarked = [("todo", false), ("review", false), ("done", false)];
        assert!(!column_is_terminal(unmarked.iter().copied(), "todo"));
        assert!(!column_is_terminal(unmarked.iter().copied(), "review"));
        assert!(column_is_terminal(unmarked.iter().copied(), "done"));

        // Explicit marks → exactly the marked columns, and the last column is no
        // longer implicitly terminal.
        let marked = [
            ("todo", false),
            ("review", true),
            ("done", true),
            ("cancelled", false),
        ];
        assert!(!column_is_terminal(marked.iter().copied(), "todo"));
        assert!(column_is_terminal(marked.iter().copied(), "review"));
        assert!(column_is_terminal(marked.iter().copied(), "done"));
        // A non-terminal *last* column (the `cancelled`-append case that broke the
        // positional rule) stays non-terminal because other columns are marked.
        assert!(!column_is_terminal(marked.iter().copied(), "cancelled"));

        // Unknown column id is never terminal.
        assert!(!column_is_terminal(marked.iter().copied(), "missing"));
        // Empty board: nothing is terminal.
        assert!(!column_is_terminal(std::iter::empty(), "done"));
    }
}
