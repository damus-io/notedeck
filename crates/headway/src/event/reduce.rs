//! The reducer: [`BoardReducer`] folds [`HeadwayEvent`]s into the
//! latest-authorised-wins overlays and finalizes them into [`BoardView`]s.

use std::collections::{HashMap, HashSet};

use nostrdb_net::NoteId;

use super::model::{
    COL_ARCHIVED, COL_DELETED, Date, Field, Priority, ReviewFields, column_is_terminal,
};
use super::parse::{
    BlockerSet, BoardEvent, CommentEvent, Container, CoverNote, FieldEdit, HeadwayEvent,
    IssueEvent, LabelSet, PlacementEvent, RelatedSet, RelationEvent, ReviewCommentEvent,
    ReviewEvent, SequenceEvent, SubjectEdit,
};
use super::view::{
    ActivityKind, ActivityView, ArchivedCard, BoardView, CardView, ColumnView, CommentView,
    EdgeRef, ReviewCommentView, ReviewView, SubissueView, subissues_all_done,
};

/// Accumulates headway events into the maps needed to resolve effective board
/// state, applying latest-authorised-wins as each event arrives. Keeping the
/// reduction incremental lets it run *inside* an [`Ndb::fold`](nostrdb::Ndb::fold) over the index
/// (see [`fold_board`](super::fold_board)) and lets the app cache a live reducer and feed it only
/// freshly-arrived notes (see [`reduce_delta`](super::reduce_delta)) instead of re-folding the whole
/// history. Both are sound because the fold is commutative and idempotent: each
/// overlay is a latest-authorised-wins map keyed by id, so an event's effect
/// doesn't depend on when (or how often) it's seen.
/// Identifies a card's placement on a specific board. The same issue placed on
/// two boards has two distinct keys (and two independent column/rank slots).
#[derive(Clone, PartialEq, Eq, Hash)]
struct PlacementKey {
    board_author: [u8; 32],
    board_id: String,
    issue_id: [u8; 32],
}

/// One live (non-deleted, non-archived) placement of a card, with its per-board
/// doneness already judged.
struct LivePlacement<'a> {
    board_author: &'a [u8; 32],
    board_id: &'a str,
    col: &'a str,
    /// Done on that board = sitting in one of its terminal columns.
    done: bool,
}

/// Where a card sits across every board it is placed on — the positional half
/// of its doneness (see [`BoardReducer::issue_done`] for the rollup half).
struct ChildPlacements<'a> {
    live: Vec<LivePlacement<'a>>,
    /// No live placement, but at least one archived one.
    archived: bool,
}

impl ChildPlacements<'_> {
    /// Positionally done: every live placement sits in a terminal column, or the
    /// card is archived everywhere it's placed.
    fn positionally_done(&self) -> bool {
        if self.live.is_empty() {
            self.archived
        } else {
            self.live.iter().all(|p| p.done)
        }
    }
}

/// Per-board cache of each card's rolled-up doneness while one board finalizes,
/// so walking a deep epic for every card and edge that names it stays linear.
type DoneMemo = HashMap<[u8; 32], bool>;

/// One raw event retained for the activity timeline: a clone of the parsed
/// event as it arrived, kept even after a newer one supersedes it in the
/// latest-wins overlays. Stored in a [`std::collections::BTreeSet`] per issue,
/// so duplicate deliveries dedupe by value (keeping ingest idempotent) and
/// iteration order is deterministic.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ActivityRecord {
    Placement(PlacementEvent),
    Subject(SubjectEdit),
    Cover(CoverNote),
    Labels(LabelSet),
    Field(FieldEdit),
    Relation(RelationEvent),
    Review(ReviewEvent),
}

impl ActivityRecord {
    fn created_at(&self) -> u64 {
        match self {
            ActivityRecord::Placement(p) => p.created_at,
            ActivityRecord::Subject(s) => s.created_at,
            ActivityRecord::Cover(c) => c.created_at,
            ActivityRecord::Labels(l) => l.created_at,
            ActivityRecord::Field(f) => f.created_at,
            ActivityRecord::Relation(r) => r.created_at,
            ActivityRecord::Review(r) => r.created_at,
        }
    }
}

/// Who may amend a card in a folded board — the axis the reducer's validity gate
/// keys off.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum Authority {
    /// Own (single-writer) board: only the card's author or the board owner may
    /// amend a card. This is [`fold_board`](super::fold_board)'s model.
    #[default]
    Owner,
    /// Shared (team) board: every holder of the team key may amend any card, so
    /// the author-identity gate is bypassed. Sound only because
    /// [`fold_shared_board`](super::fold_shared_board) pre-filters the walk to team-sealed rumors (see
    /// [`team_sealed`](super::team_sealed)) — reaching the reducer already proves the editor held the
    /// team key. Per-member edit *permissions* (an admin-signed roster) are the
    /// separate G6 gate, tracked at `headway:headway/purchase-arch-since`.
    TeamKey,
}

#[derive(Default)]
pub struct BoardReducer {
    /// The validity gate applied to overlays/placements at resolve time — see
    /// [`Authority`] and [`BoardReducer::trusts_all`].
    authority: Authority,
    /// Latest board event per (author, board_id).
    boards: HashMap<(Vec<u8>, String), BoardEvent>,
    /// Issues by id (immutable, but a relay may hand us duplicates).
    issues: HashMap<[u8; 32], IssueEvent>,
    /// Placements keyed by board + card — one per `(board, card)`. A card can be
    /// placed on several boards at once, so the key includes the board, not just
    /// the issue. Latest-authorised-wins within each key (a re-`move` on the same
    /// board supersedes the previous slot).
    placements: HashMap<PlacementKey, PlacementEvent>,
    subjects: HashMap<[u8; 32], SubjectEdit>,
    covers: HashMap<[u8; 32], CoverNote>,
    /// Latest scalar [`Field`] overlays per issue, one slot per field
    /// (latest-authorised-wins). Absent = the field was never set; the resolved
    /// typed value comes from parsing [`FieldEdit::value`] at finalize.
    fields: HashMap<[u8; 32], HashMap<Field, FieldEdit>>,
    /// Latest label set per issue. Each label event is the *complete* set for
    /// the card (snapshot semantics), so the newest authorised one wins — this
    /// is what makes label *removal* expressible: republish the set without it.
    labels: HashMap<[u8; 32], LabelSet>,
    /// Comments by comment id. Append-only — every comment is kept (unlike the
    /// latest-wins overlays above) and grouped onto its issue at finalize. Keying
    /// by comment id dedupes the duplicates a relay may hand us.
    comments: HashMap<[u8; 32], CommentEvent>,
    /// Review records by event id. Append-only like
    /// [`comments`](Self::comments) — every record is kept and grouped onto its
    /// issue at finalize, authorised there like the overlays.
    reviews: HashMap<[u8; 32], ReviewEvent>,
    /// Inline review comments by comment id, append-only like
    /// [`comments`](Self::comments). Grouped onto their review record (not the
    /// card) at finalize, so one that arrives before its record just waits in
    /// here until the record does.
    review_comments: HashMap<[u8; 32], ReviewCommentEvent>,
    /// Latest relation per *child* issue — the child's one parent slot.
    /// Latest-authorised-wins like every other overlay; authority needs the
    /// issue maps so it's checked at resolve time, not here.
    relations: HashMap<[u8; 32], RelationEvent>,
    /// Latest blocker set per *blocked* issue — its complete dependency edge set
    /// (snapshot semantics like [`labels`](Self::labels), so removal is a
    /// republish without the edge). Latest-authorised-wins; authority is checked
    /// at resolve time against the issue maps, not here.
    blockers: HashMap<[u8; 32], BlockerSet>,
    /// Latest related-to set per *storing* card — its complete related edge set on
    /// that endpoint (snapshot semantics like [`blockers`](Self::blockers), so
    /// removal is a republish without the edge). The relation is undirected, so at
    /// resolve time a card's rendered set unions its own entry with every entry
    /// naming it. Latest-authorised-wins; authority is checked at resolve time
    /// against the issue maps, not here.
    related: HashMap<[u8; 32], RelatedSet>,
    /// Latest sequence overlay per `(container, issue)` — the card's fractional
    /// work-order rank within that container (board root or parent card).
    /// Latest-authorised-wins; authority needs the issue maps so it's checked at
    /// resolve time, not here (like placements). Absent = the card is unsequenced.
    seqs: HashMap<(Container, [u8; 32]), SequenceEvent>,
    /// Full mutation history per issue, feeding the derived activity timeline
    /// ([`CardView::activity`]). The overlays above keep only the winner;
    /// nostrdb keeps every superseded event, so the fold sees them all and this
    /// set remembers them. Value-deduped, so re-ingesting stays idempotent.
    history: HashMap<[u8; 32], std::collections::BTreeSet<ActivityRecord>>,
}

impl BoardReducer {
    /// A reducer for a shared (team) board: every folded editor is trusted, so
    /// authority follows team-key possession rather than card/board authorship.
    /// Only sound when fed a team-sealed-only walk — see [`Authority::TeamKey`]
    /// and [`fold_shared_board`](super::fold_shared_board).
    pub(super) fn team_authored() -> Self {
        Self {
            authority: Authority::TeamKey,
            ..Default::default()
        }
    }

    /// Whether the author-identity validity gate is bypassed because authority is
    /// delegated to team-key possession (a shared board). Every per-overlay and
    /// per-placement gate short-circuits through this: on a shared board the fold
    /// already proved the editor held the team key, so any of them may amend.
    fn trusts_all(&self) -> bool {
        self.authority == Authority::TeamKey
    }

    /// Fold a single event into the accumulator.
    pub fn ingest(&mut self, event: HeadwayEvent) {
        match event {
            HeadwayEvent::Board(b) => {
                let key = (b.author.to_vec(), b.id.clone());
                if self
                    .boards
                    .get(&key)
                    .is_none_or(|cur| b.created_at > cur.created_at)
                {
                    self.boards.insert(key, b);
                }
            }
            HeadwayEvent::Issue(i) => {
                self.issues.insert(i.id, i);
            }
            HeadwayEvent::Placement(p) => {
                self.remember(p.issue_id, ActivityRecord::Placement(p.clone()));
                let key = PlacementKey {
                    board_author: p.board_author,
                    board_id: p.board_id.clone(),
                    issue_id: p.issue_id,
                };
                if self
                    .placements
                    .get(&key)
                    .is_none_or(|cur| newer(p.created_at, &p.author, cur.created_at, &cur.author))
                {
                    self.placements.insert(key, p);
                }
            }
            HeadwayEvent::Subject(s) => {
                self.remember(s.issue_id, ActivityRecord::Subject(s.clone()));
                if self
                    .subjects
                    .get(&s.issue_id)
                    .is_none_or(|cur| newer(s.created_at, &s.author, cur.created_at, &cur.author))
                {
                    self.subjects.insert(s.issue_id, s);
                }
            }
            HeadwayEvent::Cover(c) => {
                self.remember(c.issue_id, ActivityRecord::Cover(c.clone()));
                if self
                    .covers
                    .get(&c.issue_id)
                    .is_none_or(|cur| newer(c.created_at, &c.author, cur.created_at, &cur.author))
                {
                    self.covers.insert(c.issue_id, c);
                }
            }
            HeadwayEvent::Labels(l) => {
                self.remember(l.issue_id, ActivityRecord::Labels(l.clone()));
                if self
                    .labels
                    .get(&l.issue_id)
                    .is_none_or(|cur| newer(l.created_at, &l.author, cur.created_at, &cur.author))
                {
                    self.labels.insert(l.issue_id, l);
                }
            }
            HeadwayEvent::Field(f) => {
                self.remember(f.issue_id, ActivityRecord::Field(f.clone()));
                let slot = self.fields.entry(f.issue_id).or_default();
                if slot
                    .get(&f.field)
                    .is_none_or(|cur| newer(f.created_at, &f.author, cur.created_at, &cur.author))
                {
                    slot.insert(f.field, f);
                }
            }
            HeadwayEvent::Comment(c) => {
                // Append-only and immutable: keep the first sighting; later
                // duplicates of the same id are no-ops.
                self.comments.entry(c.id).or_insert(c);
            }
            HeadwayEvent::ReviewComment(c) => {
                self.review_comments.entry(c.id).or_insert(c);
            }
            HeadwayEvent::Review(r) => {
                // Append-only and immutable, deduped by id like a comment; also
                // remembered so the activity timeline gets a "recorded" row.
                self.remember(r.issue_id, ActivityRecord::Review(r.clone()));
                self.reviews.entry(r.id).or_insert(r);
            }
            HeadwayEvent::Relation(r) => {
                self.remember(r.child_id, ActivityRecord::Relation(r.clone()));
                if self
                    .relations
                    .get(&r.child_id)
                    .is_none_or(|cur| newer(r.created_at, &r.author, cur.created_at, &cur.author))
                {
                    self.relations.insert(r.child_id, r);
                }
            }
            HeadwayEvent::Sequence(s) => {
                // Deliberately not remembered into activity history: reseqs are
                // high-churn work-order shuffles and would bury meaningful events
                // (moves, renames) in noise. See the `birth-plate-alien` design.
                let key = (s.container.clone(), s.issue_id);
                if self
                    .seqs
                    .get(&key)
                    .is_none_or(|cur| newer(s.created_at, &s.author, cur.created_at, &cur.author))
                {
                    self.seqs.insert(key, s);
                }
            }
            HeadwayEvent::Blockers(b) => {
                // Not remembered into activity history for now: dependency edits
                // are lower-signal than moves/renames and the timeline has no
                // blocker row yet. (A future `blocked-by changed` row can add it,
                // mirroring the labels diff.)
                if self
                    .blockers
                    .get(&b.blocked_id)
                    .is_none_or(|cur| newer(b.created_at, &b.author, cur.created_at, &cur.author))
                {
                    self.blockers.insert(b.blocked_id, b);
                }
            }
            HeadwayEvent::Related(r) => {
                // Not remembered into activity history for now: like blocker
                // edits, related-to edits are lower-signal than moves/renames and
                // the timeline has no related row yet.
                if self
                    .related
                    .get(&r.card_id)
                    .is_none_or(|cur| newer(r.created_at, &r.author, cur.created_at, &cur.author))
                {
                    self.related.insert(r.card_id, r);
                }
            }
        }
    }

    /// Retain `record` on `issue`'s activity history. Value-deduped by the
    /// set, so duplicate relay deliveries are no-ops and ingest stays
    /// idempotent and commutative.
    fn remember(&mut self, issue: [u8; 32], record: ActivityRecord) {
        self.history.entry(issue).or_default().insert(record);
    }

    /// Derive `issue`'s activity timeline for the board being rendered: replay
    /// its retained history chronologically and emit a row for every visible
    /// state change (see [`ActivityKind`]). Rules that keep the timeline
    /// honest rather than noisy:
    ///
    /// - Unauthorised events are ignored, exactly like the overlays.
    /// - Records stamped at (or before) the issue's own `created_at` are part
    ///   of card creation — the write paths stamp genuine amendments strictly
    ///   later (see `store::next_after`) — so only the `Created` row shows.
    /// - The first placement is where the card started, not a move; placements
    ///   on other boards and same-column re-ranks (drag reorders) are skipped.
    /// - Label rows are the *diff* between consecutive authorised sets.
    fn card_activity(
        &self,
        issue: &IssueEvent,
        board_author: &[u8; 32],
        board_id: &str,
    ) -> Vec<ActivityView> {
        let authorised =
            |who: &[u8; 32]| self.trusts_all() || who == &issue.author || who == board_author;
        let mut out = vec![ActivityView {
            author: issue.author,
            created_at: issue.created_at,
            kind: ActivityKind::Created,
        }];
        let Some(records) = self.history.get(&issue.id) else {
            return out;
        };

        let board = self
            .boards
            .get(&(board_author.to_vec(), board_id.to_owned()));
        let col_name = |col: &str| {
            board
                .and_then(|b| b.columns.iter().find(|c| c.id == col))
                .map(|c| c.name.clone())
                .unwrap_or_else(|| col.to_owned())
        };
        let col_idx = |col: &str| board.and_then(|b| b.columns.iter().position(|c| c.id == col));

        // Chronological replay; the stable sort keeps the set's deterministic
        // order for same-second records.
        let mut sorted: Vec<&ActivityRecord> = records.iter().collect();
        sorted.sort_by_key(|r| r.created_at());

        // Running state the diffs are computed against.
        let mut prev_col: Option<&str> = None;
        let mut labels: Vec<&str> = issue.inline_labels.iter().map(String::as_str).collect();
        labels.sort_unstable();
        labels.dedup();
        let mut has_parent = false;
        // The last wire value seen per scalar field, so a field row is emitted
        // only on an actual change (an empty entry means "never set").
        let mut field_values: HashMap<Field, String> = HashMap::new();
        // Review fields already recorded, so a re-recorded commit (identical
        // fields, see `card_view`) shows only when it was first recorded.
        let mut seen_reviews: HashSet<&ReviewFields> = HashSet::new();

        for rec in sorted {
            // Creation-time records still seed the running state (so the first
            // post-creation diff is computed against them) but emit no row.
            let silent = rec.created_at() <= issue.created_at;
            match rec {
                ActivityRecord::Placement(p) => {
                    if p.board_author != *board_author
                        || p.board_id != board_id
                        || !authorised(&p.author)
                    {
                        continue;
                    }
                    let from = prev_col.replace(p.col.as_str());
                    if silent || from.is_none() || from == Some(p.col.as_str()) {
                        continue;
                    }
                    let kind = match (p.col.as_str(), from) {
                        (COL_DELETED, _) => continue,
                        (COL_ARCHIVED, _) => ActivityKind::Archived,
                        (to, Some(COL_ARCHIVED | COL_DELETED)) => ActivityKind::Restored {
                            to: col_name(to),
                            to_idx: col_idx(to),
                        },
                        (to, from) => ActivityKind::Moved {
                            from: from.map(col_name),
                            to: col_name(to),
                            to_idx: col_idx(to),
                        },
                    };
                    out.push(ActivityView {
                        author: p.author,
                        created_at: p.created_at,
                        kind,
                    });
                }
                ActivityRecord::Subject(s) => {
                    if silent || !authorised(&s.author) {
                        continue;
                    }
                    out.push(ActivityView {
                        author: s.author,
                        created_at: s.created_at,
                        kind: ActivityKind::Renamed {
                            to: s.subject.clone(),
                        },
                    });
                }
                ActivityRecord::Cover(c) => {
                    if silent || !authorised(&c.author) {
                        continue;
                    }
                    out.push(ActivityView {
                        author: c.author,
                        created_at: c.created_at,
                        kind: ActivityKind::DescriptionEdited,
                    });
                }
                ActivityRecord::Labels(l) => {
                    if !authorised(&l.author) {
                        continue;
                    }
                    let mut new: Vec<&str> = l.labels.iter().map(String::as_str).collect();
                    new.sort_unstable();
                    new.dedup();
                    let added: Vec<String> = new
                        .iter()
                        .filter(|x| !labels.contains(x))
                        .map(|x| x.to_string())
                        .collect();
                    let removed: Vec<String> = labels
                        .iter()
                        .filter(|x| !new.contains(x))
                        .map(|x| x.to_string())
                        .collect();
                    labels = new;
                    if silent || (added.is_empty() && removed.is_empty()) {
                        continue;
                    }
                    out.push(ActivityView {
                        author: l.author,
                        created_at: l.created_at,
                        kind: ActivityKind::LabelsChanged { added, removed },
                    });
                }
                ActivityRecord::Field(f) => {
                    if !authorised(&f.author) {
                        continue;
                    }
                    // Normalise "no value" so priority's explicit "none" and an
                    // empty due/estimate both read as cleared and don't churn.
                    let norm = |v: &str| match f.field {
                        Field::Priority if Priority::parse(v) == Priority::None => String::new(),
                        _ => v.trim().to_string(),
                    };
                    let to = norm(&f.value);
                    let changed = field_values.get(&f.field) != Some(&to);
                    field_values.insert(f.field, to.clone());
                    if silent || !changed {
                        continue;
                    }
                    out.push(ActivityView {
                        author: f.author,
                        created_at: f.created_at,
                        kind: ActivityKind::FieldChanged { field: f.field, to },
                    });
                }
                ActivityRecord::Relation(r) => {
                    if !self.relation_authorised(r, board_author) {
                        continue;
                    }
                    let was = has_parent;
                    has_parent = r.parent_id.is_some();
                    if silent {
                        continue;
                    }
                    let kind = match r.parent_id {
                        Some(p) => ActivityKind::ParentSet {
                            parent: NoteId::new(p),
                            title: self.card_title(&p, board_author),
                        },
                        // A detach with no prior attach says nothing.
                        None if !was => continue,
                        None => ActivityKind::ParentRemoved,
                    };
                    out.push(ActivityView {
                        author: r.author,
                        created_at: r.created_at,
                        kind,
                    });
                }
                ActivityRecord::Review(r) => {
                    if !authorised(&r.author) || !seen_reviews.insert(&r.fields) || silent {
                        continue;
                    }
                    out.push(ActivityView {
                        author: r.author,
                        created_at: r.created_at,
                        kind: ActivityKind::Review {
                            commit: r.fields.commit.clone(),
                            host: r.fields.host.clone(),
                        },
                    });
                }
            }
        }

        out
    }

    /// Resolve an issue's effective title (subject overlay applied), for
    /// naming other cards inside activity rows. `None` if the issue is unknown.
    fn card_title(&self, issue_id: &[u8; 32], board_author: &[u8; 32]) -> Option<String> {
        let issue = self.issues.get(issue_id)?;
        let authorised =
            |who: &[u8; 32]| self.trusts_all() || who == &issue.author || who == board_author;
        Some(
            self.subjects
                .get(issue_id)
                .filter(|s| authorised(&s.author))
                .map(|s| s.subject.clone())
                .unwrap_or_else(|| issue.subject.clone()),
        )
    }

    /// A relation is honoured when its author is the child's author, the named
    /// parent's author, or the board author — the authorised set of the other
    /// overlays extended to both endpoints of the edge.
    fn relation_authorised(&self, r: &RelationEvent, board_author: &[u8; 32]) -> bool {
        if self.trusts_all() || &r.author == board_author {
            return true;
        }
        if self
            .issues
            .get(&r.child_id)
            .is_some_and(|c| c.author == r.author)
        {
            return true;
        }
        r.parent_id
            .and_then(|p| self.issues.get(&p))
            .is_some_and(|p| p.author == r.author)
    }

    /// Whether a blocker set is authorised: like a relation, honoured when
    /// authored by the board owner, by the blocked card's author, or — on a shared
    /// board — any team-key holder. (The *blockers* it points at may live on other
    /// boards and belong to anyone; authority is over the blocked card's slot.)
    fn blocker_authorised(&self, b: &BlockerSet, board_author: &[u8; 32]) -> bool {
        self.trusts_all()
            || &b.author == board_author
            || self
                .issues
                .get(&b.blocked_id)
                .is_some_and(|c| c.author == b.author)
    }

    /// Whether a related-to set is authorised: like a blocker set, honoured when
    /// authored by the board owner, by the storing card's author, or — on a shared
    /// board — any team-key holder. Authority is over the storing card's slot; the
    /// partners it names may live on other boards and belong to anyone.
    fn related_authorised(&self, r: &RelatedSet, board_author: &[u8; 32]) -> bool {
        self.trusts_all()
            || &r.author == board_author
            || self
                .issues
                .get(&r.card_id)
                .is_some_and(|c| c.author == r.author)
    }

    /// The authorised relations naming `parent` as their parent — the edges that
    /// make up its direct subissues, in hash order (callers sort). Authorised
    /// against the rendered board's author like every other relation read.
    fn child_relations<'a>(
        &'a self,
        parent: &'a [u8; 32],
        board_author: &'a [u8; 32],
    ) -> impl Iterator<Item = &'a RelationEvent> + 'a {
        self.relations
            .values()
            .filter(move |r| r.parent_id.as_ref() == Some(parent))
            .filter(move |r| self.relation_authorised(r, board_author))
    }

    /// Where `child_id` sits across every board: its live placements with their
    /// per-board positional doneness, and whether it is archived somewhere.
    /// Returns `None` when the issue is unknown or has been tombstoned off every
    /// board it was placed on (it vanishes exactly like it vanishes from boards).
    fn child_placements(&self, child_id: &[u8; 32]) -> Option<ChildPlacements<'_>> {
        let child = self.issues.get(child_id)?;

        // The child's winning placements, one per board, authorised like the
        // board fold: by the child's author or that placement's board author.
        let mut placed = 0usize;
        let mut archived_somewhere = false;
        let mut live: Vec<LivePlacement> = Vec::new();

        for (key, p) in &self.placements {
            if &key.issue_id != child_id
                || (!self.trusts_all() && p.author != child.author && p.author != key.board_author)
            {
                continue;
            }
            placed += 1;
            match p.col.as_str() {
                COL_DELETED => {}
                COL_ARCHIVED => archived_somewhere = true,
                col => {
                    let done = self
                        .boards
                        .get(&(key.board_author.to_vec(), key.board_id.clone()))
                        .is_some_and(|b| {
                            column_is_terminal(
                                b.columns.iter().map(|c| (c.id.as_str(), c.terminal)),
                                col,
                            )
                        });
                    live.push(LivePlacement {
                        board_author: &key.board_author,
                        board_id: &key.board_id,
                        col,
                        done,
                    });
                }
            }
        }

        // Every placement is a tombstone: the child is deleted, drop it.
        if placed > 0 && live.is_empty() && !archived_somewhere {
            return None;
        }

        Some(ChildPlacements {
            archived: live.is_empty() && archived_somewhere,
            live,
        })
    }

    /// Is the card `id` (placed as `placements`) done? Done is positional — every
    /// live placement in a terminal column, or archived everywhere — or rolled up:
    /// the card has at least one live subissue and every one of those is done, by
    /// this same rule ([`subissues_all_done`]). So a finished epic, and an epic of
    /// finished sub-epics, is done wherever it sits, while a leaf keeps the purely
    /// positional rule.
    ///
    /// `memo` caches the rollup per card for one board's finalize (it depends on
    /// `board_author`, which authorises the relations walked). A card is marked
    /// not-done before its subtree is walked, so a subissue cycle that slipped
    /// past the write-time guard terminates instead of recursing forever.
    fn issue_done(
        &self,
        id: &[u8; 32],
        placements: &ChildPlacements,
        board_author: &[u8; 32],
        memo: &mut DoneMemo,
    ) -> bool {
        if placements.positionally_done() {
            return true;
        }
        if let Some(&done) = memo.get(id) {
            return done;
        }
        memo.insert(*id, false);

        let live_children_done = self
            .child_relations(id, board_author)
            .filter_map(|r| Some((r.child_id, self.child_placements(&r.child_id)?)))
            .filter(|(_, p)| !p.archived)
            .map(|(child, p)| self.issue_done(&child, &p, board_author, memo));
        let done = subissues_all_done(live_children_done);

        memo.insert(*id, done);
        done
    }

    /// Resolve one child of a parent card into a [`SubissueView`], deriving its
    /// doneness from its placements and its own subissues ([`Self::issue_done`]).
    /// Returns `None` when the child issue is unknown or has been tombstoned off
    /// every board it was placed on (it vanishes from the parent exactly like it
    /// vanishes from boards). `board_id`/`board_author` are the board being
    /// rendered, used to prefer its column when the child is placed on several
    /// boards.
    fn subissue_view(
        &self,
        child_id: &[u8; 32],
        board_author: &[u8; 32],
        board_id: &str,
        seq: Option<String>,
        memo: &mut DoneMemo,
    ) -> Option<SubissueView> {
        let child = self.issues.get(child_id)?;
        let authorised =
            |who: &[u8; 32]| self.trusts_all() || who == &child.author || who == board_author;

        let title = self
            .subjects
            .get(child_id)
            .filter(|s| authorised(&s.author))
            .map(|s| s.subject.clone())
            .unwrap_or_else(|| child.subject.clone());

        let mut placements = self.child_placements(child_id)?;
        let done = self.issue_done(child_id, &placements, board_author, memo);

        // Prefer the rendered board's column; else the first by board id so the
        // result doesn't churn with hash order.
        placements
            .live
            .sort_by(|a, b| (a.board_author, a.board_id).cmp(&(b.board_author, b.board_id)));
        let column = placements
            .live
            .iter()
            .find(|p| p.board_author == board_author && p.board_id == board_id)
            .or_else(|| placements.live.first())
            .map(|p| p.col.to_owned());

        Some(SubissueView {
            id: NoteId::new(*child_id),
            title,
            column,
            done,
            archived: placements.archived,
            seq,
        })
    }

    /// Resolve a card's effective content (title, description, labels, comments)
    /// from the issue and its overlay events, given the `rank`/`placed_at` of the
    /// placement it's being shown under. `board_author` is the authority alongside
    /// the card author for amend events. Board-agnostic: the same issue placed on
    /// two boards yields the same content, only the rank/slot differ (`board_id`
    /// is only a display preference for subissue columns, not authority).
    fn card_view(
        &self,
        issue: &IssueEvent,
        board_author: &[u8; 32],
        board_id: &str,
        rank: String,
        placed_at: u64,
        memo: &mut DoneMemo,
    ) -> CardView {
        // Authority: the card author or the board author may amend the card (or,
        // on a shared board, any team-key holder — see `BoardReducer::trusts_all`).
        let authorised =
            |who: &[u8; 32]| self.trusts_all() || who == &issue.author || who == board_author;

        let subject = self
            .subjects
            .get(&issue.id)
            .filter(|s| authorised(&s.author));
        let title = subject
            .map(|s| s.subject.clone())
            .unwrap_or_else(|| issue.subject.clone());

        let cover = self.covers.get(&issue.id).filter(|c| authorised(&c.author));
        let description = cover
            .map(|c| c.body.clone())
            .unwrap_or_else(|| issue.body.clone());

        // Labels resolve latest-authorised-wins: the newest authorised label
        // event is the card's complete set, overriding the issue's inline labels.
        // (Removal = republish the set without the label.)
        let label_set = self.labels.get(&issue.id).filter(|s| authorised(&s.author));
        let mut labels = label_set
            .map(|s| s.labels.clone())
            .unwrap_or_else(|| issue.inline_labels.clone());
        labels.sort();
        labels.dedup();

        // Scalar field overlays (priority/due/estimate) resolve
        // latest-authorised-wins from the per-issue field slots; an unauthorised
        // edit is ignored, leaving the field unset. Each value is parsed into its
        // typed form here at the read site.
        let field_slots = self.fields.get(&issue.id);
        let field = |f: Field| {
            field_slots
                .and_then(|m| m.get(&f))
                .filter(|e| authorised(&e.author))
        };
        let priority = field(Field::Priority).map_or(Priority::None, |e| Priority::parse(&e.value));
        let due = field(Field::Due).and_then(|e| Date::parse(&e.value));
        let estimate = field(Field::Estimate).and_then(|e| e.value.trim().parse::<u32>().ok());
        // Newest authorised field edit, folded into `updated_at` below.
        let fields_touched = field_slots.map_or(0, |m| {
            m.values()
                .filter(|e| authorised(&e.author))
                .map(|e| e.created_at)
                .max()
                .unwrap_or(0)
        });

        // Comments thread under the issue (the NIP-22 root). Append-only, shown
        // oldest first; the id breaks same-second ties.
        let mut comments: Vec<CommentView> = self
            .comments
            .values()
            .filter(|c| c.issue_id == issue.id)
            .map(|c| CommentView {
                id: NoteId::new(c.id),
                author: c.author,
                parent: c.parent_id.map(NoteId::new),
                body: c.body.clone(),
                created_at: c.created_at,
            })
            .collect();
        comments.sort_by(|a, b| (a.created_at, a.id.bytes()).cmp(&(b.created_at, b.id.bytes())));

        // Review records on the card, authorised like the overlays (a stranger
        // can't attach a commit to someone else's card). Newest first, so the
        // head is the latest commit; the id breaks same-second ties.
        let mut records: Vec<&ReviewEvent> = self
            .reviews
            .values()
            .filter(|r| r.issue_id == issue.id && authorised(&r.author))
            .collect();
        records.sort_by_key(|r| std::cmp::Reverse((r.created_at, r.id)));
        // A retried done step re-records the same commit with the same fields;
        // records are append-only, so collapse identical fields here and keep
        // the newest. Any differing field (explainer, host, path) is new
        // information and survives as its own record.
        //
        // A collapsed duplicate may still carry review comments (its id was
        // shown until the newer copy arrived), so its comments follow it onto
        // the record that survives.
        let mut survivors: HashMap<&ReviewFields, usize> = HashMap::new();
        let mut owner_of: HashMap<[u8; 32], usize> = HashMap::new();
        let mut reviews: Vec<ReviewView> = Vec::new();
        for r in records {
            let at = *survivors.entry(&r.fields).or_insert_with(|| {
                reviews.push(ReviewView {
                    id: NoteId::new(r.id),
                    author: r.author,
                    created_at: r.created_at,
                    fields: r.fields.clone(),
                    comments: Vec::new(),
                });
                reviews.len() - 1
            });
            owner_of.insert(r.id, at);
        }
        for c in self.review_comments.values() {
            let Some(&at) = owner_of.get(&c.record_id) else {
                continue;
            };
            reviews[at].comments.push(ReviewCommentView {
                id: NoteId::new(c.id),
                author: c.author,
                parent: c.parent_id.map(NoteId::new),
                location: c.location.clone(),
                body: c.body.clone(),
                created_at: c.created_at,
            });
        }
        for r in &mut reviews {
            r.comments
                .sort_by(|a, b| (a.created_at, a.id.bytes()).cmp(&(b.created_at, b.id.bytes())));
        }

        // The newest touch wins: creation, the winning amendments, the last
        // comment or the latest review record. Placements deliberately don't
        // count (see the field docs).
        let updated_at = issue
            .created_at
            .max(subject.map_or(0, |s| s.created_at))
            .max(cover.map_or(0, |c| c.created_at))
            .max(label_set.map_or(0, |l| l.created_at))
            .max(fields_touched)
            .max(comments.last().map_or(0, |c| c.created_at))
            .max(reviews.first().map_or(0, |r| r.created_at))
            .max(
                reviews
                    .iter()
                    .filter_map(|r| r.comments.last())
                    .map(|c| c.created_at)
                    .max()
                    .unwrap_or(0),
            );

        // This card as a child: its one relation slot names its parent.
        let parent = self
            .relations
            .get(&issue.id)
            .filter(|r| self.relation_authorised(r, board_author))
            .and_then(|r| r.parent_id)
            .map(NoteId::new);

        // This card as a parent: every issue whose authorised relation names it.
        // One level only — a cycle renders as two cards pointing at each other,
        // never a loop (the write path refuses to create one; see store::apply).
        let children: Vec<&RelationEvent> = self.child_relations(&issue.id, board_author).collect();
        // Each child's work-order rank is scoped to THIS card as its container,
        // authorised like the relation edge: the child's author, this parent's
        // author, or the board author may sequence it.
        let child_seq = |child_id: &[u8; 32]| -> Option<String> {
            let entry = self.seqs.get(&(Container::Card(issue.id), *child_id))?;
            let child_author = self.issues.get(child_id).map(|c| c.author);
            let ok = self.trusts_all()
                || &entry.author == board_author
                || child_author == Some(entry.author)
                || entry.author == issue.author;
            ok.then(|| entry.rank.clone())
        };
        let mut children: Vec<(&RelationEvent, Option<String>)> = children
            .into_iter()
            .map(|r| {
                let seq = child_seq(&r.child_id);
                (r, seq)
            })
            .collect();
        // Sequenced children lead in rank order; unsequenced fall back to creation
        // order (`created_at`, then id). `is_none()` sorts false < true, so a
        // sequenced (`Some`) child always precedes an unsequenced (`None`) one.
        children.sort_by_cached_key(|(r, seq)| {
            let created = self
                .issues
                .get(&r.child_id)
                .map_or(u64::MAX, |c| c.created_at);
            (
                seq.is_none(),
                seq.clone().unwrap_or_default(),
                created,
                r.child_id,
            )
        });
        let subissues = children
            .into_iter()
            .filter_map(|(r, seq)| {
                self.subissue_view(&r.child_id, board_author, board_id, seq, memo)
            })
            .collect();

        // Board-root work-order rank for this card, authorised like its own
        // overlays (the card author or the board author may sequence it).
        let seq = self
            .seqs
            .get(&(Container::BoardRoot(board_id.to_string()), issue.id))
            .filter(|e| authorised(&e.author))
            .map(|e| e.rank.clone());

        // Resolve one dependency edge to the referenced card's title + cleared
        // state, reusing the subissue doneness logic (terminal column / archived,
        // or every live subissue done). `None` when the referenced card is unknown
        // or tombstoned off every board — a vanished card no longer participates
        // in the edge.
        let mut edge = |id: &[u8; 32]| {
            self.subissue_view(id, board_author, board_id, None, memo)
                .map(|s| EdgeRef {
                    id: s.id,
                    title: s.title,
                    done: s.done,
                })
        };

        // This card as blocked: its authorised blocker set, in listed order.
        let blocked_by = self
            .blockers
            .get(&issue.id)
            .filter(|b| self.blocker_authorised(b, board_author))
            .map(|b| b.blockers.iter().filter_map(&mut edge).collect())
            .unwrap_or_default();

        // This card as a blocker: the reverse edges — every authorised set that
        // names it — sorted by id so the order doesn't churn with hash iteration.
        let mut blocks: Vec<EdgeRef> = self
            .blockers
            .values()
            .filter(|b| b.blockers.contains(&issue.id))
            .filter(|b| self.blocker_authorised(b, board_author))
            .filter_map(|b| edge(&b.blocked_id))
            .collect();
        blocks.sort_by(|a, b| a.id.bytes().cmp(b.id.bytes()));

        // This card's related-to edges. The relation is undirected, so the set
        // unions both directions — the card's own stored set and every authorised
        // set that names it — collecting the partner ids first, then resolving.
        // Deduped by id (a pair related from both endpoints) and skipping the card
        // itself, then sorted by id so hash iteration order doesn't churn the
        // output. Purely informational: never consulted by readiness or ordering.
        let mut partner_ids: Vec<[u8; 32]> = Vec::new();
        if let Some(set) = self
            .related
            .get(&issue.id)
            .filter(|r| self.related_authorised(r, board_author))
        {
            partner_ids.extend(set.related.iter().copied());
        }
        for set in self.related.values() {
            if set.related.contains(&issue.id) && self.related_authorised(set, board_author) {
                partner_ids.push(set.card_id);
            }
        }
        let mut seen: HashSet<[u8; 32]> = HashSet::new();
        let mut related: Vec<EdgeRef> = partner_ids
            .into_iter()
            .filter(|id| *id != issue.id && seen.insert(*id))
            .filter_map(|id| edge(&id))
            .collect();
        related.sort_by(|a, b| a.id.bytes().cmp(b.id.bytes()));

        CardView {
            id: NoteId::new(issue.id),
            author: issue.author,
            title,
            description,
            labels,
            priority,
            due,
            estimate,
            rank,
            seq,
            placed_at,
            created_at: issue.created_at,
            updated_at,
            comments,
            reviews,
            activity: self.card_activity(issue, board_author, board_id),
            parent,
            subissues,
            blocked_by,
            blocks,
            related,
        }
    }

    /// Assemble the accumulated events into board views.
    /// Resolve the accumulated events into the boards they describe. Takes
    /// `&self` so a cached reducer can be re-finalized after a delta without
    /// being consumed.
    #[profiling::function]
    pub fn finalize(&self) -> Vec<BoardView> {
        let mut views: Vec<BoardView> = Vec::new();

        // Issues with a live placement on *some* board. An issue with none is a
        // placement-less orphan (e.g. its placement event never reached us) and
        // is shown via its origin board's `a` tag below; one that was explicitly
        // moved/deleted has a placement and so is governed purely by placements.
        let placed_anywhere: HashSet<[u8; 32]> =
            self.placements.keys().map(|k| k.issue_id).collect();

        for ((author, board_id), board) in &self.boards {
            // Group this board's cards by resolved column id.
            let mut by_col: HashMap<String, Vec<CardView>> = HashMap::new();
            let mut fallback: Vec<(u64, CardView)> = Vec::new();
            let mut archived: Vec<ArchivedCard> = Vec::new();
            let col_ids: Vec<&str> = board.columns.iter().map(|c| c.id.as_str()).collect();
            // Rolled-up doneness depends on this board's author (it authorises the
            // relations walked), so the cache lives for one board only.
            let mut done_memo = DoneMemo::new();

            // Placement-driven membership: each live placement targeting this
            // board puts its issue on the board, in the placement's column.
            for (key, placement) in &self.placements {
                if key.board_author.as_slice() != author.as_slice() || &key.board_id != board_id {
                    continue;
                }
                let Some(issue) = self.issues.get(&key.issue_id) else {
                    continue;
                };
                // Only the card author or the board author may place a card (or,
                // on a shared board, any team-key holder — see `trusts_all`).
                if !self.trusts_all()
                    && placement.author != issue.author
                    && placement.author != board.author
                {
                    continue;
                }

                let card = self.card_view(
                    issue,
                    &board.author,
                    board_id,
                    placement.rank.clone(),
                    placement.created_at,
                    &mut done_memo,
                );

                match placement.col.as_str() {
                    // A tombstone placement removes the card from the board.
                    COL_DELETED => continue,
                    // Archived: kept off the columns but recoverable, with its
                    // origin column so a restore lands it back where it was.
                    COL_ARCHIVED => archived.push(ArchivedCard {
                        card,
                        from: placement.from.clone(),
                    }),
                    col if col_ids.contains(&col) => {
                        by_col.entry(col.to_string()).or_default().push(card);
                    }
                    _ => fallback.push((issue.created_at, card)),
                }
            }

            // Orphan fallback: issues anchored to this board by their `a` tag but
            // with no placement on any board (a lost placement event). Show them
            // so a card never vanishes just because its placement didn't arrive.
            for issue in self.issues.values() {
                if issue.board_author.as_slice() != author.as_slice()
                    || &issue.board_id != board_id
                    || placed_anywhere.contains(&issue.id)
                {
                    continue;
                }
                let card = self.card_view(
                    issue,
                    &board.author,
                    board_id,
                    String::new(),
                    0,
                    &mut done_memo,
                );
                fallback.push((issue.created_at, card));
            }

            let mut columns: Vec<ColumnView> = board
                .columns
                .iter()
                .map(|def| {
                    let mut cards = by_col.remove(&def.id).unwrap_or_default();
                    cards.sort_by(|a, b| a.rank.cmp(&b.rank));
                    ColumnView {
                        id: def.id.clone(),
                        name: def.name.clone(),
                        terminal: def.terminal,
                        cards,
                    }
                })
                .collect();

            // Unplaced cards fall into the first column, oldest first.
            if let Some(first) = columns.first_mut() {
                fallback.sort_by_key(|(created, _)| *created);
                first
                    .cards
                    .extend(fallback.into_iter().map(|(_, card)| card));
            }

            // Stable order so the archived view and snapshots don't churn.
            archived.sort_by(|a, b| a.card.id.bytes().cmp(b.card.id.bytes()));

            views.push(BoardView {
                id: board_id.clone(),
                author: board.author,
                title: board.title.clone(),
                description: board.description.clone(),
                created_at: board.created_at,
                columns,
                archived,
            });
        }

        // Stable output order: by board id.
        views.sort_by(|a, b| a.id.cmp(&b.id));
        views
    }
}

/// Resolve a set of headway events into the boards they describe.
///
/// For each board the latest board event (by `created_at`) wins. Cards are
/// placed by their latest *authorised* placement (`col` + `rank`), with title /
/// description / labels resolved the same way. Cards with no placement, or whose
/// placement points at an unknown column, fall into the first column, ordered by
/// creation time after the explicitly placed cards.
pub fn reduce(events: &[HeadwayEvent]) -> Vec<BoardView> {
    let mut reducer = BoardReducer::default();
    for event in events {
        reducer.ingest(event.clone());
    }
    reducer.finalize()
}

/// "Latest authorised wins" comparator: newer `created_at` wins, ties broken by
/// author bytes so the result is deterministic.
fn newer(a_at: u64, a_who: &[u8; 32], b_at: u64, b_who: &[u8; 32]) -> bool {
    (a_at, a_who) > (b_at, b_who)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::parse::tests::note_id;
    use nostrdb_net::FullKeypair;

    use nostrdb::NoteBuilder;

    use crate::event::build::{
        build_archive_placement, build_blockers, build_board, build_comment, build_cover_note,
        build_field, build_issue, build_labels, build_placement, build_related, build_relation,
        build_review, build_review_comment, build_sequence, build_subject_edit,
    };
    use crate::event::model::{ColumnDef, LineSide, ReviewFields, ReviewLocation, board_address};
    use crate::event::parse::parse;

    /// Build a full board (board + two issues + placements) and reduce it,
    /// checking columns, ordering and the metadata overrides.
    #[test]
    fn reduce_builds_board_view() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];

        let mut events = Vec::new();
        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        events.push(parse_owned(build_board("b1", "Board", "", &cols), &owner));

        let i1 = note_id(&owner, build_issue(&addr, "First", ""));
        let i2 = note_id(&owner, build_issue(&addr, "Second", ""));
        events.push(parse_owned(build_issue(&addr, "First", ""), &owner));
        events.push(parse_owned(build_issue(&addr, "Second", ""), &owner));

        // Both into "todo": i2 ranked before i1.
        events.push(parse_owned(
            build_placement("b1", &addr, &i1, "todo", "t"),
            &owner,
        ));
        events.push(parse_owned(
            build_placement("b1", &addr, &i2, "todo", "g"),
            &owner,
        ));
        // Rename i1, label it, give it a description.
        events.push(parse_owned(
            build_subject_edit(&i1, "First (edited)"),
            &owner,
        ));
        events.push(parse_owned(build_labels(&i1, &["bug".to_string()]), &owner));
        events.push(parse_owned(
            build_cover_note(&i1, &owner.pubkey, "details"),
            &owner,
        ));

        let views = reduce(&events);
        assert_eq!(views.len(), 1);
        let view = &views[0];
        assert_eq!(view.columns.len(), 2);

        let todo = &view.columns[0];
        assert_eq!(todo.id, "todo");
        // Sorted by rank ascending: "g" (Second) before "t" (First).
        assert_eq!(todo.cards.len(), 2);
        assert_eq!(todo.cards[0].title, "Second");
        assert_eq!(todo.cards[1].title, "First (edited)");
        assert_eq!(todo.cards[1].labels, vec!["bug".to_string()]);
        assert_eq!(todo.cards[1].description, "details");

        assert!(view.columns[1].cards.is_empty());
    }

    /// `created_at` pins to the immutable issue event; `updated_at` follows the
    /// newest amendment or comment and doesn't count placements (moves are
    /// tracked by `placed_at`).
    #[test]
    fn reduce_resolves_card_timestamps() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        // Explicit timestamps make the issue id (and the fold) deterministic.
        let i1 = note_id(&owner, build_issue(&addr, "First", "").created_at(1_000));
        let mut events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "First", "").created_at(1_000), &owner),
            parse_owned(
                build_placement("b1", &addr, &i1, "todo", "m").created_at(5_000),
                &owner,
            ),
        ];

        // Untouched card: updated_at falls back to creation, and the (later)
        // placement doesn't drag it forward.
        let card = reduce(&events)[0].columns[0].cards[0].clone();
        assert_eq!(card.created_at, 1_000);
        assert_eq!(card.updated_at, 1_000);

        // A rename bumps updated_at without moving created_at.
        events.push(parse_owned(
            build_subject_edit(&i1, "Renamed").created_at(2_000),
            &owner,
        ));
        let card = reduce(&events)[0].columns[0].cards[0].clone();
        assert_eq!(card.created_at, 1_000);
        assert_eq!(card.updated_at, 2_000);

        // A comment counts as an update too.
        events.push(parse_owned(
            build_comment(&i1, &owner.pubkey, None, "hi").created_at(3_000),
            &owner,
        ));
        assert_eq!(reduce(&events)[0].columns[0].cards[0].updated_at, 3_000);
    }

    /// The activity timeline replays a card's full history: creation-time
    /// events are silent (only `Created` shows), then every move, rename,
    /// label diff, description edit, archive/restore and parent change gets a
    /// chronological row. Unauthorised events and same-column re-ranks don't.
    #[test]
    fn reduce_derives_activity_timeline() {
        let owner = FullKeypair::generate();
        let stranger = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("doing", "Doing"),
            ColumnDef::new("done", "Done"),
        ];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let card = note_id(&owner, build_issue(&addr, "Card", "").created_at(1_000));
        let parent = note_id(&owner, build_issue(&addr, "Epic", "").created_at(900));
        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Epic", "").created_at(900), &owner),
            parse_owned(build_issue(&addr, "Card", "").created_at(1_000), &owner),
            // Creation-time placement + labels: part of creation, no rows.
            parse_owned(
                build_placement("b1", &addr, &card, "todo", "m").created_at(1_000),
                &owner,
            ),
            parse_owned(build_labels(&card, &["bug"]).created_at(1_000), &owner),
            // The history proper, one event per second.
            parse_owned(
                build_placement("b1", &addr, &card, "doing", "m").created_at(2_000),
                &owner,
            ),
            // Same-column re-rank (drag reorder): no row.
            parse_owned(
                build_placement("b1", &addr, &card, "doing", "t").created_at(2_500),
                &owner,
            ),
            parse_owned(
                build_subject_edit(&card, "Card v2").created_at(3_000),
                &owner,
            ),
            // A stranger's rename is ignored, exactly like the overlays.
            parse_owned(
                build_subject_edit(&card, "hijacked").created_at(3_500),
                &stranger,
            ),
            parse_owned(
                build_labels(&card, &["bug", "ui"]).created_at(4_000),
                &owner,
            ),
            parse_owned(build_labels(&card, &["ui"]).created_at(5_000), &owner),
            parse_owned(
                build_cover_note(&card, &owner.pubkey, "details").created_at(6_000),
                &owner,
            ),
            parse_owned(
                build_archive_placement("b1", &addr, &card, "doing", "t").created_at(7_000),
                &owner,
            ),
            parse_owned(
                build_placement("b1", &addr, &card, "doing", "t").created_at(8_000),
                &owner,
            ),
            parse_owned(
                build_relation(&card, Some(&parent)).created_at(9_000),
                &owner,
            ),
            parse_owned(build_relation(&card, None).created_at(10_000), &owner),
        ];

        let view = &reduce(&events)[0];
        let card = view.columns[1]
            .cards
            .iter()
            .find(|c| c.id == card)
            .expect("card in doing");

        let kinds: Vec<&ActivityKind> = card.activity.iter().map(|a| &a.kind).collect();
        assert_eq!(
            kinds,
            vec![
                &ActivityKind::Created,
                &ActivityKind::Moved {
                    from: Some("Todo".into()),
                    to: "Doing".into(),
                    to_idx: Some(1),
                },
                &ActivityKind::Renamed {
                    to: "Card v2".into()
                },
                &ActivityKind::LabelsChanged {
                    added: vec!["ui".into()],
                    removed: vec![],
                },
                &ActivityKind::LabelsChanged {
                    added: vec![],
                    removed: vec!["bug".into()],
                },
                &ActivityKind::DescriptionEdited,
                &ActivityKind::Archived,
                &ActivityKind::Restored {
                    to: "Doing".into(),
                    to_idx: Some(1),
                },
                &ActivityKind::ParentSet {
                    parent,
                    title: Some("Epic".into()),
                },
                &ActivityKind::ParentRemoved,
            ]
        );
        // Rows are chronological and stamped with the underlying events' times.
        assert_eq!(card.activity[0].created_at, 1_000);
        assert_eq!(card.activity[1].created_at, 2_000);
        assert!(
            card.activity
                .windows(2)
                .all(|w| w[0].created_at <= w[1].created_at)
        );
    }

    #[test]
    fn reduce_ignores_unauthorised_edits() {
        let owner = FullKeypair::generate();
        let stranger = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let i1 = note_id(&owner, build_issue(&addr, "Original", ""));
        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Original", ""), &owner),
            parse_owned(build_placement("b1", &addr, &i1, "todo", "m"), &owner),
            // A stranger tries to rename the card: must be ignored.
            parse_owned(build_subject_edit(&i1, "Hijacked"), &stranger),
        ];

        let views = reduce(&events);
        assert_eq!(views[0].columns[0].cards[0].title, "Original");
    }

    /// Labels are snapshot/latest-wins, not an additive union: republishing the
    /// set without a label removes it. The newer (whole) set must win.
    #[test]
    fn reduce_label_removal_replaces_the_set() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let i1 = note_id(&owner, build_issue(&addr, "Card", ""));

        let mut events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Card", ""), &owner),
            parse_owned(build_placement("b1", &addr, &i1, "todo", "m"), &owner),
            parse_owned(
                build_labels(&i1, &["bug".to_string(), "ux".to_string()]),
                &owner,
            ),
        ];

        // Republish the set without "bug" — a later event so it wins latest-wins.
        let mut shrunk = match parse_owned(build_labels(&i1, &["ux".to_string()]), &owner) {
            HeadwayEvent::Labels(l) => l,
            _ => unreachable!(),
        };
        shrunk.created_at += 1;
        events.push(HeadwayEvent::Labels(shrunk));

        let views = reduce(&events);
        // "bug" is gone; only "ux" remains (not a union of both).
        assert_eq!(views[0].columns[0].cards[0].labels, vec!["ux".to_string()]);
    }

    #[test]
    fn reduce_resolves_scalar_fields_latest_authorised() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let i1 = note_id(&owner, build_issue(&addr, "Card", ""));

        let mut events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Card", ""), &owner),
            parse_owned(build_placement("b1", &addr, &i1, "todo", "m"), &owner),
            parse_owned(
                build_field(&i1, Field::Priority, Priority::Low.as_str()),
                &owner,
            ),
            parse_owned(build_field(&i1, Field::Due, "2026-07-30"), &owner),
            parse_owned(build_field(&i1, Field::Estimate, "3"), &owner),
        ];
        let card = |events: &[HeadwayEvent]| reduce(events)[0].columns[0].cards[0].clone();
        let c = card(&events);
        assert_eq!(c.priority, Priority::Low);
        assert_eq!(c.due.unwrap().to_string(), "2026-07-30");
        assert_eq!(c.estimate, Some(3));

        // A later priority overlay wins latest-wins — raise it to Urgent.
        let mut bumped = match parse_owned(
            build_field(&i1, Field::Priority, Priority::Urgent.as_str()),
            &owner,
        ) {
            HeadwayEvent::Field(f) => f,
            _ => unreachable!(),
        };
        bumped.created_at += 1;
        events.push(HeadwayEvent::Field(bumped));
        assert_eq!(card(&events).priority, Priority::Urgent);

        // Clearing one field republishes an empty value; fields are independent.
        let mut cleared = match parse_owned(build_field(&i1, Field::Due, ""), &owner) {
            HeadwayEvent::Field(f) => f,
            _ => unreachable!(),
        };
        cleared.created_at += 2;
        events.push(HeadwayEvent::Field(cleared));
        let c = card(&events);
        assert_eq!(c.due, None);
        assert_eq!(c.priority, Priority::Urgent); // other fields untouched
        assert_eq!(c.estimate, Some(3));
    }

    /// Comments fold onto their card oldest-first, deduped by id, and a reply
    /// keeps its parent link.
    #[test]
    fn reduce_attaches_comments_to_cards() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let i1 = note_id(&owner, build_issue(&addr, "Card", ""));

        // Two comments and a reply; stamp increasing created_at so order is fixed.
        let comment_id = |kp: &FullKeypair, b: NoteBuilder| {
            NoteId::new(*b.sign(&kp.secret_key.secret_bytes()).build().unwrap().id())
        };
        let c1 = comment_id(&owner, build_comment(&i1, &owner.pubkey, None, "one"));

        let stamp = |ev: HeadwayEvent, at: u64| match ev {
            HeadwayEvent::Comment(mut c) => {
                c.created_at = at;
                HeadwayEvent::Comment(c)
            }
            other => other,
        };

        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Card", ""), &owner),
            parse_owned(build_placement("b1", &addr, &i1, "todo", "m"), &owner),
            stamp(
                parse_owned(build_comment(&i1, &owner.pubkey, None, "one"), &owner),
                10,
            ),
            stamp(
                parse_owned(build_comment(&i1, &owner.pubkey, None, "two"), &owner),
                20,
            ),
            stamp(
                parse_owned(
                    build_comment(&i1, &owner.pubkey, Some((&c1, &owner.pubkey)), "re: one"),
                    &owner,
                ),
                30,
            ),
        ];

        let views = reduce(&events);
        let card = &views[0].columns[0].cards[0];
        assert_eq!(card.comments.len(), 3);
        // Oldest first.
        assert_eq!(card.comments[0].body, "one");
        assert_eq!(card.comments[1].body, "two");
        assert_eq!(card.comments[2].body, "re: one");
        // The reply points back at the first comment; top-level ones don't.
        assert_eq!(card.comments[0].parent, None);
        assert_eq!(card.comments[2].parent, Some(c1));
    }

    /// A review record naming `commit` recorded on `host`.
    fn review_on(commit: &str, host: &str) -> ReviewFields {
        ReviewFields {
            commit: Some(commit.to_string()),
            host: Some(host.to_string()),
            ..Default::default()
        }
    }

    /// Two review records on one card from different hosts both fold in —
    /// append-only, not latest-wins — newest first, bump `updated_at`, and each
    /// gets an activity row. A duplicate delivery of one is kept once.
    #[test]
    fn reduce_attaches_review_records_newest_first() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let issue = || build_issue(&addr, "Card", "").created_at(1_000);
        let i1 = note_id(&owner, issue());
        let older = parse_owned(
            build_review(&i1, &review_on("aaaa", "jex0")).created_at(2_000),
            &owner,
        );
        let newer = parse_owned(
            build_review(&i1, &review_on("bbbb", "quiver")).created_at(3_000),
            &owner,
        );

        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(issue(), &owner),
            parse_owned(build_placement("b1", &addr, &i1, "todo", "m"), &owner),
            older.clone(),
            newer,
            older,
        ];

        let views = reduce(&events);
        let card = &views[0].columns[0].cards[0];
        let hosts: Vec<_> = card
            .reviews
            .iter()
            .map(|r| r.fields.host.as_deref())
            .collect();
        assert_eq!(hosts, vec![Some("quiver"), Some("jex0")]);
        assert_eq!(card.reviews[0].fields.commit.as_deref(), Some("bbbb"));
        assert_eq!(card.updated_at, 3_000);

        let rows: Vec<&ActivityKind> = card.activity.iter().map(|a| &a.kind).collect();
        assert_eq!(
            rows,
            vec![
                &ActivityKind::Created,
                &ActivityKind::Review {
                    commit: Some("aaaa".into()),
                    host: Some("jex0".into()),
                },
                &ActivityKind::Review {
                    commit: Some("bbbb".into()),
                    host: Some("quiver".into()),
                },
            ]
        );
    }

    /// A retried done step re-records identical fields: the card keeps only
    /// the newest copy, `updated_at` follows it, and the activity timeline
    /// shows the commit once, when it was first recorded.
    #[test]
    fn reduce_collapses_identical_review_records() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let issue = || build_issue(&addr, "Card", "").created_at(1_000);
        let i1 = note_id(&owner, issue());
        let a = review_on("aaaa", "jex0");
        let b = review_on("bbbb", "jex0");
        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(issue(), &owner),
            parse_owned(build_placement("b1", &addr, &i1, "todo", "m"), &owner),
            parse_owned(build_review(&i1, &a).created_at(2_000), &owner),
            parse_owned(build_review(&i1, &b).created_at(3_000), &owner),
            parse_owned(build_review(&i1, &a).created_at(4_000), &owner),
        ];

        let views = reduce(&events);
        let card = &views[0].columns[0].cards[0];
        let kept: Vec<_> = card
            .reviews
            .iter()
            .map(|r| (r.fields.commit.as_deref(), r.created_at))
            .collect();
        assert_eq!(kept, vec![(Some("aaaa"), 4_000), (Some("bbbb"), 3_000)]);
        assert_eq!(card.updated_at, 4_000);

        let rows: Vec<&ActivityKind> = card.activity.iter().map(|a| &a.kind).collect();
        assert_eq!(
            rows,
            vec![
                &ActivityKind::Created,
                &ActivityKind::Review {
                    commit: Some("aaaa".into()),
                    host: Some("jex0".into()),
                },
                &ActivityKind::Review {
                    commit: Some("bbbb".into()),
                    host: Some("jex0".into()),
                },
            ]
        );
        assert_eq!(card.activity[1].created_at, 2_000, "first recording");
    }

    /// The same commit re-recorded with a new explainer carries new
    /// information, so both records survive.
    #[test]
    fn reduce_keeps_same_commit_with_different_explainer() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let issue = || build_issue(&addr, "Card", "").created_at(1_000);
        let i1 = note_id(&owner, issue());
        let plain = review_on("aaaa", "jex0");
        let explained = ReviewFields {
            explainer: Some("https://claude.ai/artifact/x".to_string()),
            ..plain.clone()
        };
        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(issue(), &owner),
            parse_owned(build_placement("b1", &addr, &i1, "todo", "m"), &owner),
            parse_owned(build_review(&i1, &plain).created_at(2_000), &owner),
            parse_owned(build_review(&i1, &explained).created_at(3_000), &owner),
        ];

        let views = reduce(&events);
        let card = &views[0].columns[0].cards[0];
        assert_eq!(card.reviews.len(), 2);
        assert_eq!(card.reviews[0].fields, explained);
        assert_eq!(card.reviews[1].fields, plain);
        let review_rows = card
            .activity
            .iter()
            .filter(|a| matches!(a.kind, ActivityKind::Review { .. }))
            .count();
        assert_eq!(review_rows, 2);
    }

    /// On an owner board a stranger's copy of the owner's record neither
    /// counts as the newest copy nor hides the owner's own record.
    #[test]
    fn reduce_unauthorised_duplicate_review_hides_nothing() {
        let owner = FullKeypair::generate();
        let stranger = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let issue = || build_issue(&addr, "Card", "").created_at(1_000);
        let i1 = note_id(&owner, issue());
        let a = review_on("aaaa", "jex0");
        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(issue(), &owner),
            parse_owned(build_placement("b1", &addr, &i1, "todo", "m"), &owner),
            // The stranger's copy comes first and last, bracketing the owner's.
            parse_owned(build_review(&i1, &a).created_at(1_500), &stranger),
            parse_owned(build_review(&i1, &a).created_at(2_000), &owner),
            parse_owned(build_review(&i1, &a).created_at(3_000), &stranger),
        ];

        let views = reduce(&events);
        let card = &views[0].columns[0].cards[0];
        assert_eq!(card.reviews.len(), 1);
        assert_eq!(&card.reviews[0].author, owner.pubkey.bytes());
        assert_eq!(card.reviews[0].created_at, 2_000);
        assert_eq!(card.updated_at, 2_000);
        assert_eq!(card.activity.len(), 2, "Created + the owner's Review");
        assert_eq!(card.activity[1].created_at, 2_000);
    }

    /// On a single-writer board a stranger can't attach a commit to someone
    /// else's card: their record is dropped from both the card and its activity.
    #[test]
    fn reduce_ignores_unauthorised_review_records() {
        let owner = FullKeypair::generate();
        let stranger = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let issue = || build_issue(&addr, "Card", "").created_at(1_000);
        let i1 = note_id(&owner, issue());
        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(issue(), &owner),
            parse_owned(build_placement("b1", &addr, &i1, "todo", "m"), &owner),
            parse_owned(
                build_review(&i1, &review_on("cccc", "evil")).created_at(2_000),
                &stranger,
            ),
        ];

        let views = reduce(&events);
        let card = &views[0].columns[0].cards[0];
        assert!(card.reviews.is_empty());
        assert_eq!(card.updated_at, 1_000);
        assert_eq!(card.activity.len(), 1, "only the Created row");
    }

    /// Inline review comments fold onto their review record, oldest first, and
    /// stay out of the card's own comments; a card comment still lands on the
    /// card. A review comment that arrives before its record waits for it, and
    /// one on a record collapsed as a duplicate follows it onto the survivor.
    #[test]
    fn reduce_attaches_review_comments_to_their_record() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];
        let sign = |b: NoteBuilder| {
            let note = b.sign(&owner.secret_key.secret_bytes()).build().unwrap();
            (NoteId::new(*note.id()), parse(&note).unwrap())
        };
        let loc = |start: u32| ReviewLocation {
            path: "src/lib.rs".into(),
            commit: "aaaa".into(),
            start,
            end: start,
            side: LineSide::New,
        };

        let (i1, issue) = sign(build_issue(&addr, "Card", "").created_at(1_000));
        let fields = review_on("aaaa", "jex0");
        let (r_old, record_old) = sign(build_review(&i1, &fields).created_at(2_000));
        let (_, record_new) = sign(build_review(&i1, &fields).created_at(3_000));
        let (_, second) = sign(
            build_review_comment(&r_old, &owner.pubkey, None, Some(&loc(9)), "second")
                .created_at(5_000),
        );
        let (_, first) = sign(
            build_review_comment(&r_old, &owner.pubkey, None, Some(&loc(3)), "first")
                .created_at(4_000),
        );
        let (_, card_comment) =
            sign(build_comment(&i1, &owner.pubkey, None, "on the card").created_at(1_500));

        // The comments come first: nothing to attach to until the records land.
        let events = vec![
            second,
            first,
            parse_owned_board(&owner, &cols),
            issue,
            sign(build_placement("b1", &addr, &i1, "todo", "m")).1,
            record_old,
            record_new,
            card_comment,
        ];
        let views = reduce(&events);
        let card = &views[0].columns[0].cards[0];

        assert_eq!(card.reviews.len(), 1, "identical records collapse");
        let bodies: Vec<&str> = card.reviews[0]
            .comments
            .iter()
            .map(|c| c.body.as_str())
            .collect();
        assert_eq!(bodies, vec!["first", "second"]);
        assert_eq!(card.reviews[0].comments[0].location, Some(loc(3)));
        let card_bodies: Vec<&str> = card.comments.iter().map(|c| c.body.as_str()).collect();
        assert_eq!(card_bodies, vec!["on the card"]);
        assert_eq!(card.updated_at, 5_000);
    }

    /// The board definition `owner` signs for a one-board test.
    fn parse_owned_board(owner: &FullKeypair, cols: &[ColumnDef]) -> HeadwayEvent {
        let note = build_board("b1", "Board", "", cols)
            .sign(&owner.secret_key.secret_bytes())
            .build()
            .unwrap();
        parse(&note).unwrap()
    }

    /// A relay may hand us the same comment twice; the reducer keeps one.
    #[test]
    fn reduce_dedupes_duplicate_comments() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let i1 = note_id(&owner, build_issue(&addr, "Card", ""));
        let comment = parse_owned(build_comment(&i1, &owner.pubkey, None, "dup"), &owner);

        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Card", ""), &owner),
            parse_owned(build_placement("b1", &addr, &i1, "todo", "m"), &owner),
            comment.clone(),
            comment,
        ];

        let views = reduce(&events);
        assert_eq!(views[0].columns[0].cards[0].comments.len(), 1);
    }

    #[test]
    fn reduce_skips_deleted_cards() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let keep = note_id(&owner, build_issue(&addr, "Keep", ""));
        let gone = note_id(&owner, build_issue(&addr, "Gone", ""));

        let mut events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Keep", ""), &owner),
            parse_owned(build_issue(&addr, "Gone", ""), &owner),
            parse_owned(build_placement("b1", &addr, &keep, "todo", "m"), &owner),
            parse_owned(build_placement("b1", &addr, &gone, "todo", "t"), &owner),
        ];

        // Tombstone the second card with a later placement.
        let mut tombstone = match parse_owned(
            build_placement("b1", &addr, &gone, COL_DELETED, "t"),
            &owner,
        ) {
            HeadwayEvent::Placement(p) => p,
            _ => unreachable!(),
        };
        // Ensure the tombstone wins the latest-wins race deterministically.
        tombstone.created_at += 1;
        events.push(HeadwayEvent::Placement(tombstone));

        let views = reduce(&events);
        let cards = &views[0].columns[0].cards;
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].title, "Keep");
    }

    /// Membership is placement-driven: one issue placed on two boards appears on
    /// both, and removing the placement from one board leaves it on the other
    /// (the same card, not a copy).
    #[test]
    fn reduce_places_one_card_on_multiple_boards() {
        let owner = FullKeypair::generate();
        let addr1 = board_address(&owner.pubkey, "b1");
        let addr2 = board_address(&owner.pubkey, "b2");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        // Anchored to b1, but placed on both b1 and b2 (the second is a "link").
        let card = note_id(&owner, build_issue(&addr1, "Shared", ""));
        let mut events = vec![
            parse_owned(build_board("b1", "One", "", &cols), &owner),
            parse_owned(build_board("b2", "Two", "", &cols), &owner),
            parse_owned(build_issue(&addr1, "Shared", ""), &owner),
            parse_owned(build_placement("b1", &addr1, &card, "todo", "m"), &owner),
            parse_owned(build_placement("b2", &addr2, &card, "todo", "m"), &owner),
        ];

        // Views sort by board id: [b1, b2]. The card shows on both.
        let views = reduce(&events);
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].columns[0].cards.len(), 1, "on b1");
        assert_eq!(views[1].columns[0].cards.len(), 1, "on b2");
        assert_eq!(views[1].columns[0].cards[0].title, "Shared");

        // Remove it from b1 (a tombstone placement on b1 only). b2 keeps it.
        let mut tombstone = match parse_owned(
            build_placement("b1", &addr1, &card, COL_DELETED, "m"),
            &owner,
        ) {
            HeadwayEvent::Placement(p) => p,
            _ => unreachable!(),
        };
        tombstone.created_at += 1;
        events.push(HeadwayEvent::Placement(tombstone));

        let views = reduce(&events);
        assert!(views[0].columns[0].cards.is_empty(), "removed from b1");
        assert_eq!(views[1].columns[0].cards.len(), 1, "still on b2");
    }

    #[test]
    fn reduce_archives_cards_with_their_origin() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let card = note_id(&owner, build_issue(&addr, "Shelve me", ""));

        let mut events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Shelve me", ""), &owner),
            parse_owned(build_placement("b1", &addr, &card, "done", "m"), &owner),
        ];

        // Archive it from "done" with a later placement so it wins latest-wins.
        let mut archive = match parse_owned(
            build_archive_placement("b1", &addr, &card, "done", "m"),
            &owner,
        ) {
            HeadwayEvent::Placement(p) => p,
            _ => unreachable!(),
        };
        archive.created_at += 1;
        events.push(HeadwayEvent::Placement(archive));

        let views = reduce(&events);
        // Gone from every column, present in `archived` with its origin recorded.
        assert!(views[0].columns.iter().all(|c| c.cards.is_empty()));
        assert_eq!(views[0].archived.len(), 1);
        assert_eq!(views[0].archived[0].card.title, "Shelve me");
        assert_eq!(views[0].archived[0].from.as_deref(), Some("done"));
    }

    #[test]
    fn reduce_falls_back_unplaced_cards_to_first_column() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Loose card", ""), &owner),
        ];

        let views = reduce(&events);
        assert_eq!(views[0].columns[0].cards.len(), 1);
        assert_eq!(views[0].columns[0].cards[0].title, "Loose card");
    }

    /// Parent/child resolve on both ends: the child gains a `parent` pointer and
    /// the parent lists its children with doneness derived from their columns
    /// (last column of the board = done).
    #[test]
    fn reduce_resolves_subissues_with_positional_doneness() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let epic = note_id(&owner, build_issue(&addr, "Epic", "").created_at(1_000));
        let c1 = note_id(
            &owner,
            build_issue(&addr, "Child one", "").created_at(1_001),
        );
        let c2 = note_id(
            &owner,
            build_issue(&addr, "Child two", "").created_at(1_002),
        );

        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Epic", "").created_at(1_000), &owner),
            parse_owned(
                build_issue(&addr, "Child one", "").created_at(1_001),
                &owner,
            ),
            parse_owned(
                build_issue(&addr, "Child two", "").created_at(1_002),
                &owner,
            ),
            parse_owned(build_placement("b1", &addr, &epic, "todo", "g"), &owner),
            // c1 done (last column), c2 still in todo.
            parse_owned(build_placement("b1", &addr, &c1, "done", "m"), &owner),
            parse_owned(build_placement("b1", &addr, &c2, "todo", "t"), &owner),
            parse_owned(build_relation(&c1, Some(&epic)), &owner),
            parse_owned(build_relation(&c2, Some(&epic)), &owner),
        ];

        let views = reduce(&events);
        let todo = &views[0].columns[0];

        let epic_card = todo.cards.iter().find(|c| c.id == epic).unwrap();
        assert_eq!(epic_card.parent, None);
        assert_eq!(epic_card.subissues.len(), 2);
        // Ordered by child created_at: c1 (done) then c2 (not).
        assert_eq!(epic_card.subissues[0].title, "Child one");
        assert!(epic_card.subissues[0].done);
        assert_eq!(epic_card.subissues[0].column.as_deref(), Some("done"));
        assert_eq!(epic_card.subissues[1].title, "Child two");
        assert!(!epic_card.subissues[1].done);
        assert_eq!(epic_card.subissues[1].column.as_deref(), Some("todo"));

        let child = todo.cards.iter().find(|c| c.id == c2).unwrap();
        assert_eq!(child.parent, Some(epic));
        assert!(child.subissues.is_empty());
    }

    /// A terminal column that is *not* the last column still makes a card in it
    /// count as done — its subissue rollup reads done and it clears a blocker,
    /// even though a later (non-terminal) column exists. The board carries the
    /// `terminal` flag through build → parse.
    #[test]
    fn reduce_terminal_column_clears_before_last() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        // `in-review` is terminal though `done` sits after it.
        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("in-review", "In Review").terminal(),
            ColumnDef::new("done", "Done").terminal(),
        ];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        // The board round-trips the terminal flag through the wire.
        let HeadwayEvent::Board(board) = parse_owned(build_board("b1", "Board", "", &cols), &owner)
        else {
            panic!("board");
        };
        assert_eq!(
            board.columns.iter().map(|c| c.terminal).collect::<Vec<_>>(),
            vec![false, true, true],
        );

        let a = note_id(&owner, build_issue(&addr, "A", "").created_at(1_000));
        let b = note_id(&owner, build_issue(&addr, "B", "").created_at(1_001));

        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "A", "").created_at(1_000), &owner),
            parse_owned(build_issue(&addr, "B", "").created_at(1_001), &owner),
            parse_owned(
                build_placement("b1", &addr, &a, "todo", "m").created_at(1_100),
                &owner,
            ),
            // B sits in the terminal `in-review` column — not the last column.
            parse_owned(
                build_placement("b1", &addr, &b, "in-review", "m").created_at(1_100),
                &owner,
            ),
            // A is blocked by B, and B is A's subissue.
            parse_owned(build_blockers(&a, &[b]).created_at(1_200), &owner),
            parse_owned(build_relation(&b, Some(&a)).created_at(1_200), &owner),
        ];

        let view = &reduce(&events)[0];
        // B is in a terminal column → the folded view reads it as done.
        assert!(view.card_is_done(b));
        assert!(view.column_is_terminal("in-review"));
        // A's blocker edge is cleared, so A is not blocked.
        let card_a = view.card(a).unwrap();
        assert!(card_a.blocked_by[0].done);
        assert!(!card_a.is_blocked());
        // The subissue rollup counts B done.
        assert_eq!(card_a.subissues.len(), 1);
        assert!(card_a.subissues[0].done);
    }

    /// A dependency edge resolves on both ends: the blocked card gains a
    /// `blocked_by` edge with the blocker's positional doneness, the blocker gains
    /// the reverse `blocks` edge, and clearing the blocker (moving it to the last
    /// column) or republishing an empty set flips/removes the edge.
    #[test]
    fn reduce_resolves_blocker_edges() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let a = note_id(&owner, build_issue(&addr, "A", "").created_at(1_000));
        let b = note_id(&owner, build_issue(&addr, "B", "").created_at(1_001));

        let mut events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "A", "").created_at(1_000), &owner),
            parse_owned(build_issue(&addr, "B", "").created_at(1_001), &owner),
            parse_owned(
                build_placement("b1", &addr, &a, "todo", "m").created_at(1_100),
                &owner,
            ),
            parse_owned(
                build_placement("b1", &addr, &b, "todo", "m").created_at(1_100),
                &owner,
            ),
            // A is blocked by B.
            parse_owned(build_blockers(&a, &[b]).created_at(1_200), &owner),
        ];

        let view = &reduce(&events)[0];
        let card_a = view.card(a).unwrap();
        let card_b = view.card(b).unwrap();
        // A's edge names B, unfinished (B is in "todo", not the last column).
        assert_eq!(card_a.blocked_by.len(), 1);
        assert_eq!(card_a.blocked_by[0].id, b);
        assert!(!card_a.blocked_by[0].done);
        assert!(card_a.is_blocked());
        // Reverse edge: B blocks A; B itself is unblocked.
        assert_eq!(
            card_b.blocks.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![a]
        );
        assert!(card_b.blocked_by.is_empty());
        assert!(!card_b.is_blocked());

        // Move B into "done" (the last column): the edge is now cleared.
        events.push(parse_owned(
            build_placement("b1", &addr, &b, "done", "m").created_at(2_000),
            &owner,
        ));
        let view = &reduce(&events)[0];
        let card_a = view.card(a).unwrap();
        assert!(card_a.blocked_by[0].done);
        assert!(!card_a.is_blocked());

        // Republish A's set empty: the edge is gone.
        events.push(parse_owned(
            build_blockers(&a, &[]).created_at(3_000),
            &owner,
        ));
        let view = &reduce(&events)[0];
        assert!(view.card(a).unwrap().blocked_by.is_empty());
    }

    /// A card blocked on an epic whose subissues are all done is unblocked and
    /// ready, while the finished epic stays in its column but is never ready
    /// itself. An epic whose only subissue is archived is a leaf, not done, so
    /// it keeps holding its blocked card back.
    #[test]
    fn reduce_clears_blockers_on_a_finished_epic() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };
        let issue = |title: &str, at: u64| build_issue(&addr, title, "").created_at(at);

        let epic = note_id(&owner, issue("Epic", 1_000));
        let c1 = note_id(&owner, issue("C1", 1_001));
        let c2 = note_id(&owner, issue("C2", 1_002));
        let waiting = note_id(&owner, issue("Waiting", 1_003));
        let shelf = note_id(&owner, issue("Shelf", 1_004));
        let shelved = note_id(&owner, issue("Shelved", 1_005));
        let stuck = note_id(&owner, issue("Stuck", 1_006));

        let mut events = vec![parse_owned(build_board("b1", "Board", "", &cols), &owner)];
        for (title, at) in [
            ("Epic", 1_000),
            ("C1", 1_001),
            ("C2", 1_002),
            ("Waiting", 1_003),
            ("Shelf", 1_004),
            ("Shelved", 1_005),
            ("Stuck", 1_006),
        ] {
            events.push(parse_owned(issue(title, at), &owner));
        }
        for (id, col) in [
            (&epic, "todo"),
            (&c1, "done"),
            (&c2, "done"),
            (&waiting, "todo"),
            (&shelf, "todo"),
            (&stuck, "todo"),
        ] {
            events.push(parse_owned(
                build_placement("b1", &addr, id, col, "m"),
                &owner,
            ));
        }
        events.extend([
            parse_owned(
                build_archive_placement("b1", &addr, &shelved, "done", "m"),
                &owner,
            ),
            parse_owned(build_relation(&c1, Some(&epic)), &owner),
            parse_owned(build_relation(&c2, Some(&epic)), &owner),
            parse_owned(build_relation(&shelved, Some(&shelf)), &owner),
            parse_owned(build_blockers(&waiting, &[epic]), &owner),
            parse_owned(build_blockers(&stuck, &[shelf]), &owner),
        ]);

        let view = &reduce(&events)[0];
        // The epic is done by rollup but stays where it sits.
        assert!(view.card_is_done(epic));
        assert!(view.columns[0].cards.iter().any(|c| c.id == epic));
        // Its blocked card is cleared.
        let card = view.card(waiting).unwrap();
        assert!(card.blocked_by[0].done);
        assert!(!card.is_blocked());
        // Only-archived subissues make a leaf: positional, so not done.
        assert!(!view.card_is_done(shelf));
        assert!(view.card(stuck).unwrap().is_blocked());

        let root = Container::BoardRoot("b1".to_string());
        let ready: Vec<NoteId> = crate::traversal::ready(view, &root)
            .iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ready, vec![waiting, shelf]);
    }

    /// Done rolls up through nesting: an epic whose only subissue is a sub-epic
    /// of finished cards is done, clears what it blocks, and is never ready.
    #[test]
    fn reduce_rolls_doneness_up_through_nested_epics() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };
        let issue = |title: &str, at: u64| build_issue(&addr, title, "").created_at(at);

        let top = note_id(&owner, issue("Top", 1_000));
        let sub_epic = note_id(&owner, issue("Sub-epic", 1_001));
        let leaf = note_id(&owner, issue("Leaf", 1_002));
        let after = note_id(&owner, issue("After", 1_003));

        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(issue("Top", 1_000), &owner),
            parse_owned(issue("Sub-epic", 1_001), &owner),
            parse_owned(issue("Leaf", 1_002), &owner),
            parse_owned(issue("After", 1_003), &owner),
            parse_owned(build_placement("b1", &addr, &top, "todo", "a"), &owner),
            parse_owned(build_placement("b1", &addr, &sub_epic, "todo", "b"), &owner),
            parse_owned(build_placement("b1", &addr, &leaf, "done", "m"), &owner),
            parse_owned(build_placement("b1", &addr, &after, "todo", "c"), &owner),
            parse_owned(build_relation(&sub_epic, Some(&top)), &owner),
            parse_owned(build_relation(&leaf, Some(&sub_epic)), &owner),
            parse_owned(build_blockers(&after, &[top]), &owner),
        ];

        let view = &reduce(&events)[0];
        assert!(view.card_is_done(sub_epic));
        assert!(view.card_is_done(top));
        assert!(view.card(top).unwrap().subissues[0].done);
        assert!(!view.card(after).unwrap().is_blocked());

        let root = Container::BoardRoot("b1".to_string());
        let ready: Vec<NoteId> = crate::traversal::ready(view, &root)
            .iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ready, vec![after]);
        assert!(crate::traversal::ready(view, &Container::Card(*top.bytes())).is_empty());
    }

    /// A blocker set from someone who is neither the blocked card's author nor the
    /// board owner is ignored, exactly like every other overlay.
    #[test]
    fn reduce_ignores_unauthorised_blockers() {
        let owner = FullKeypair::generate();
        let stranger = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let a = note_id(&owner, build_issue(&addr, "A", "").created_at(1_000));
        let b = note_id(&owner, build_issue(&addr, "B", "").created_at(1_001));

        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "A", "").created_at(1_000), &owner),
            parse_owned(build_issue(&addr, "B", "").created_at(1_001), &owner),
            parse_owned(
                build_placement("b1", &addr, &a, "todo", "m").created_at(1_100),
                &owner,
            ),
            parse_owned(
                build_placement("b1", &addr, &b, "todo", "m").created_at(1_100),
                &owner,
            ),
            // The stranger tries to declare A blocked by B — unauthorised.
            parse_owned(build_blockers(&a, &[b]).created_at(1_200), &stranger),
        ];

        let view = &reduce(&events)[0];
        assert!(view.card(a).unwrap().blocked_by.is_empty());
        assert!(!view.card(a).unwrap().is_blocked());
    }

    /// A related-to edge is undirected: storing it on one endpoint surfaces it on
    /// both cards' `related` sets, it never feeds `is_blocked`/readiness, relating
    /// the pair from the other endpoint too doesn't duplicate it, and republishing
    /// an empty set removes it from both ends.
    #[test]
    fn reduce_resolves_related_edges_symmetrically() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let a = note_id(&owner, build_issue(&addr, "A", "").created_at(1_000));
        let b = note_id(&owner, build_issue(&addr, "B", "").created_at(1_001));

        let mut events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "A", "").created_at(1_000), &owner),
            parse_owned(build_issue(&addr, "B", "").created_at(1_001), &owner),
            parse_owned(
                build_placement("b1", &addr, &a, "todo", "m").created_at(1_100),
                &owner,
            ),
            parse_owned(
                build_placement("b1", &addr, &b, "todo", "m").created_at(1_100),
                &owner,
            ),
            // A relates to B — stored on A's endpoint only.
            parse_owned(build_related(&a, &[b]).created_at(1_200), &owner),
        ];

        let view = &reduce(&events)[0];
        let card_a = view.card(a).unwrap();
        let card_b = view.card(b).unwrap();
        // Both endpoints list each other, despite the edge living on A alone.
        assert_eq!(
            card_a.related.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![b]
        );
        assert_eq!(
            card_b.related.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![a]
        );
        // Purely informational: never touches blocking/readiness.
        assert!(card_a.blocked_by.is_empty());
        assert!(!card_a.is_blocked());
        assert!(!card_b.is_blocked());

        // Redundantly relate the same pair from B: the reverse-scan union dedupes,
        // so neither side gains a duplicate entry.
        events.push(parse_owned(
            build_related(&b, &[a]).created_at(1_300),
            &owner,
        ));
        let view = &reduce(&events)[0];
        assert_eq!(view.card(a).unwrap().related.len(), 1);
        assert_eq!(view.card(b).unwrap().related.len(), 1);

        // Clear both stored sets: the relation is gone from both ends.
        events.push(parse_owned(
            build_related(&a, &[]).created_at(2_000),
            &owner,
        ));
        events.push(parse_owned(
            build_related(&b, &[]).created_at(2_001),
            &owner,
        ));
        let view = &reduce(&events)[0];
        assert!(view.card(a).unwrap().related.is_empty());
        assert!(view.card(b).unwrap().related.is_empty());
    }

    /// A related-to set from someone who is neither the storing card's author nor
    /// the board owner is ignored, exactly like every other overlay.
    #[test]
    fn reduce_ignores_unauthorised_related() {
        let owner = FullKeypair::generate();
        let stranger = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let a = note_id(&owner, build_issue(&addr, "A", "").created_at(1_000));
        let b = note_id(&owner, build_issue(&addr, "B", "").created_at(1_001));

        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "A", "").created_at(1_000), &owner),
            parse_owned(build_issue(&addr, "B", "").created_at(1_001), &owner),
            parse_owned(
                build_placement("b1", &addr, &a, "todo", "m").created_at(1_100),
                &owner,
            ),
            parse_owned(
                build_placement("b1", &addr, &b, "todo", "m").created_at(1_100),
                &owner,
            ),
            // The stranger tries to relate A to B — unauthorised over A's slot.
            parse_owned(build_related(&a, &[b]).created_at(1_200), &stranger),
        ];

        let view = &reduce(&events)[0];
        assert!(view.card(a).unwrap().related.is_empty());
        assert!(view.card(b).unwrap().related.is_empty());
    }

    /// Subissues sort by sequence: sequenced children lead in rank order, then
    /// unsequenced ones fall back to creation order.
    #[test]
    fn reduce_orders_subissues_by_sequence() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];
        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };
        let epic = note_id(&owner, build_issue(&addr, "Epic", "").created_at(1_000));
        let c1 = note_id(
            &owner,
            build_issue(&addr, "Child one", "").created_at(1_001),
        );
        let c2 = note_id(
            &owner,
            build_issue(&addr, "Child two", "").created_at(1_002),
        );
        let c3 = note_id(
            &owner,
            build_issue(&addr, "Child three", "").created_at(1_003),
        );

        let epic_container = Container::Card(*epic.bytes());
        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Epic", "").created_at(1_000), &owner),
            parse_owned(
                build_issue(&addr, "Child one", "").created_at(1_001),
                &owner,
            ),
            parse_owned(
                build_issue(&addr, "Child two", "").created_at(1_002),
                &owner,
            ),
            parse_owned(
                build_issue(&addr, "Child three", "").created_at(1_003),
                &owner,
            ),
            parse_owned(build_placement("b1", &addr, &epic, "todo", "g"), &owner),
            parse_owned(build_placement("b1", &addr, &c1, "todo", "h"), &owner),
            parse_owned(build_placement("b1", &addr, &c2, "todo", "i"), &owner),
            parse_owned(build_placement("b1", &addr, &c3, "todo", "j"), &owner),
            parse_owned(build_relation(&c1, Some(&epic)), &owner),
            parse_owned(build_relation(&c2, Some(&epic)), &owner),
            parse_owned(build_relation(&c3, Some(&epic)), &owner),
            // Sequence c3 before c2 within the epic; leave c1 unsequenced.
            parse_owned(build_sequence(&epic_container, &c3, "g"), &owner),
            parse_owned(build_sequence(&epic_container, &c2, "m"), &owner),
        ];

        let views = reduce(&events);
        let epic_card = views[0].columns[0]
            .cards
            .iter()
            .find(|c| c.id == epic)
            .unwrap();
        let order: Vec<&str> = epic_card
            .subissues
            .iter()
            .map(|s| s.title.as_str())
            .collect();
        assert_eq!(order, ["Child three", "Child two", "Child one"]);
        assert_eq!(epic_card.subissues[0].seq.as_deref(), Some("g"));
        assert_eq!(epic_card.subissues[1].seq.as_deref(), Some("m"));
        assert_eq!(epic_card.subissues[2].seq, None);
    }

    /// A board-root sequence overlay resolves onto a top-level card's `seq`,
    /// leaving its column `rank` untouched (independent axes).
    #[test]
    fn reduce_resolves_board_root_sequence() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];
        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };
        let card = note_id(&owner, build_issue(&addr, "Card", "").created_at(1_000));
        let root = Container::BoardRoot("b1".into());
        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Card", "").created_at(1_000), &owner),
            parse_owned(build_placement("b1", &addr, &card, "todo", "m"), &owner),
            parse_owned(build_sequence(&root, &card, "an"), &owner),
        ];
        let views = reduce(&events);
        let cv = views[0].columns[0]
            .cards
            .iter()
            .find(|c| c.id == card)
            .unwrap();
        assert_eq!(cv.seq.as_deref(), Some("an"));
        assert_eq!(cv.rank, "m");
    }

    /// A newer authorised sequence supersedes an older one; a stranger's newer
    /// sequence shadows the slot but is ignored at resolve (like other overlays).
    #[test]
    fn sequence_latest_authorised_wins_and_ignores_strangers() {
        let owner = FullKeypair::generate();
        let stranger = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];
        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };
        let card = note_id(&owner, build_issue(&addr, "Card", "").created_at(1_000));
        let root = Container::BoardRoot("b1".into());
        let mut events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Card", "").created_at(1_000), &owner),
            parse_owned(build_placement("b1", &addr, &card, "todo", "m"), &owner),
            parse_owned(build_sequence(&root, &card, "g").created_at(2_000), &owner),
        ];
        let find_seq = |evs: &[HeadwayEvent]| -> Option<String> {
            reduce(evs)[0].columns[0]
                .cards
                .iter()
                .find(|c| c.id == card)
                .unwrap()
                .seq
                .clone()
        };
        assert_eq!(find_seq(&events).as_deref(), Some("g"));

        // Newer authorised reseq wins.
        events.push(parse_owned(
            build_sequence(&root, &card, "t").created_at(3_000),
            &owner,
        ));
        assert_eq!(find_seq(&events).as_deref(), Some("t"));

        // A stranger's even-newer seq shadows the slot but isn't honoured.
        events.push(parse_owned(
            build_sequence(&root, &card, "z").created_at(4_000),
            &stranger,
        ));
        assert_eq!(find_seq(&events), None, "stranger ignored");
    }

    /// The relation slot is latest-authorised-wins: a newer relation re-parents,
    /// a newer detach clears, and a stranger's relation is ignored outright.
    #[test]
    fn reduce_reparents_latest_wins_and_ignores_strangers() {
        let owner = FullKeypair::generate();
        let stranger = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let e1 = note_id(&owner, build_issue(&addr, "Epic 1", "").created_at(1_000));
        let e2 = note_id(&owner, build_issue(&addr, "Epic 2", "").created_at(1_001));
        let child = note_id(&owner, build_issue(&addr, "Child", "").created_at(1_002));

        let mut events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Epic 1", "").created_at(1_000), &owner),
            parse_owned(build_issue(&addr, "Epic 2", "").created_at(1_001), &owner),
            parse_owned(build_issue(&addr, "Child", "").created_at(1_002), &owner),
            parse_owned(build_placement("b1", &addr, &e1, "todo", "g"), &owner),
            parse_owned(build_placement("b1", &addr, &e2, "todo", "m"), &owner),
            parse_owned(build_placement("b1", &addr, &child, "todo", "t"), &owner),
            parse_owned(build_relation(&child, Some(&e1)).created_at(2_000), &owner),
        ];

        let find = |views: &Vec<BoardView>, id: NoteId| -> CardView {
            views[0].columns[0]
                .cards
                .iter()
                .find(|c| c.id == id)
                .unwrap()
                .clone()
        };

        // A stranger's relation must not re-parent the card. (Like every
        // overlay, ingest is authority-blind and authority is applied at
        // resolve: the stranger's newer event shadows the owner's older slot
        // rather than losing to it, so the card reads as unparented — but the
        // hijack itself never takes effect.)
        events.push(parse_owned(
            build_relation(&child, Some(&e2)).created_at(3_000),
            &stranger,
        ));
        let views = reduce(&events);
        assert_eq!(find(&views, child).parent, None, "stranger ignored");
        assert!(find(&views, e2).subissues.is_empty(), "hijack inert");

        // The owner re-parents: newest authorised slot wins on both ends.
        events.push(parse_owned(
            build_relation(&child, Some(&e2)).created_at(4_000),
            &owner,
        ));
        let views = reduce(&events);
        assert_eq!(find(&views, child).parent, Some(e2));
        assert!(find(&views, e1).subissues.is_empty());
        assert_eq!(find(&views, e2).subissues.len(), 1);

        // And a detach (no parent tag) clears it.
        events.push(parse_owned(
            build_relation(&child, None).created_at(5_000),
            &owner,
        ));
        let views = reduce(&events);
        assert_eq!(find(&views, child).parent, None);
        assert!(find(&views, e2).subissues.is_empty());
    }

    /// An archived-everywhere child counts as done (filed away); a tombstoned
    /// child drops off its parent's subissue list entirely.
    #[test]
    fn reduce_subissue_doneness_for_archived_and_deleted_children() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];

        let parse_owned = |b: NoteBuilder, kp: &FullKeypair| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let epic = note_id(&owner, build_issue(&addr, "Epic", "").created_at(1_000));
        let shelved = note_id(&owner, build_issue(&addr, "Shelved", "").created_at(1_001));
        let gone = note_id(&owner, build_issue(&addr, "Gone", "").created_at(1_002));

        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols), &owner),
            parse_owned(build_issue(&addr, "Epic", "").created_at(1_000), &owner),
            parse_owned(build_issue(&addr, "Shelved", "").created_at(1_001), &owner),
            parse_owned(build_issue(&addr, "Gone", "").created_at(1_002), &owner),
            parse_owned(build_placement("b1", &addr, &epic, "todo", "g"), &owner),
            parse_owned(
                build_archive_placement("b1", &addr, &shelved, "done", "m"),
                &owner,
            ),
            parse_owned(
                build_placement("b1", &addr, &gone, COL_DELETED, "t"),
                &owner,
            ),
            parse_owned(build_relation(&shelved, Some(&epic)), &owner),
            parse_owned(build_relation(&gone, Some(&epic)), &owner),
        ];

        let views = reduce(&events);
        let epic_card = views[0].columns[0]
            .cards
            .iter()
            .find(|c| c.id == epic)
            .unwrap();

        // The deleted child vanished; the archived one counts as done.
        assert_eq!(epic_card.subissues.len(), 1);
        assert_eq!(epic_card.subissues[0].title, "Shelved");
        assert!(epic_card.subissues[0].done);
        assert!(epic_card.subissues[0].archived);
        assert_eq!(epic_card.subissues[0].column, None);
    }
}
