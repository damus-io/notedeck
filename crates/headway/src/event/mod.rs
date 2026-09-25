//! Nostr event model for headway boards.
//!
//! Cards are NIP-34 issues (kind 1621) anchored to a headway *board* (a custom
//! addressable kind). Because the issue event is immutable, everything mutable
//! about a card — its title, labels, description and which column it sits in —
//! lives in *separate* events:
//!
//! | concept           | kind    | mechanism                                  |
//! | ----------------- | ------- | ------------------------------------------ |
//! | board             | `30619` | addressable; `d` = board id, ordered `col` |
//! | card              | `1621`  | NIP-34 issue, `a` → board                  |
//! | title edit        | `1985`  | NIP-32 label, `L`/`l` namespace `#subject` |
//! | labels            | `1985`  | NIP-32 label, `L`/`l` namespace `#t`       |
//! | description edit  | `1624`  | gitworkshop cover note                     |
//! | placement         | `30620` | addressable; `col` + fractional `rank`     |
//! | relation          | `30621` | addressable; `d` = child, `parent` tag     |
//! | sequence          | `30622` | addressable; `d` = `<container>:<issue>`   |
//! | blockers          | `30624` | addressable; `d` = blocked, `blocked-by` tags |
//! | related           | `30625` | addressable; `d` = a card, `related` tags |
//!
//! Effective state is resolved as **latest-authorised-wins** for every overlay
//! (placement, subject, cover note, and labels — each label event carries the
//! card's complete set, so the newest one wins), where "authorised"
//! means the event's author is the card author or the board's author
//! (maintainer). This mirrors the ngitstack/gitworkshop "Shared Issue / Patch /
//! PR Metadata" spec.
//!
//! This module is pure: it builds and parses notes and reduces a set of them
//! into a [`BoardView`]. Relay/ndb plumbing lives in the app layer.

use nostrdb::{Filter, Ndb, Note, NoteKey, Transaction};
use nostrdb_net::{NoteId, Pubkey};

mod build;
mod json;
mod kinds;
mod model;
mod parse;
mod reduce;
mod view;

pub use build::{
    build_archive_placement, build_blockers, build_board, build_board_pref, build_comment,
    build_cover_note, build_field, build_issue, build_labels, build_placement, build_related,
    build_relation, build_sequence, build_subject_edit,
};
pub use json::{activity_json, board_json, card_json, comment_json};
pub use kinds::{
    HEADWAY_KINDS, KIND_BLOCKERS, KIND_BOARD, KIND_BOARD_PREF, KIND_COMMENT, KIND_COVER_NOTE,
    KIND_ISSUE, KIND_LABEL, KIND_PLACEMENT, KIND_RELATED, KIND_RELATION, KIND_SEQUENCE,
    is_addressable,
};
pub use model::{
    BoardCoord, COL_ARCHIVED, COL_DELETED, ColumnDef, Date, Field, Priority, board_address,
    column_is_terminal,
};
pub use parse::{
    BlockerSet, BoardEvent, CommentEvent, Container, CoverNote, FieldEdit, HeadwayEvent,
    IssueEvent, LabelSet, PlacementEvent, RelatedSet, RelationEvent, SequenceEvent, SubjectEdit,
    parse,
};
use parse::{parse_blockers, parse_related};
pub use reduce::{BoardReducer, reduce};
pub use view::{
    ActivityKind, ActivityView, ArchivedCard, BoardView, CardView, ColumnView, CommentView,
    EdgeRef, SubissueView,
};

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

/// A filter for `author`'s board-preference note (kind 30623). Kind 30623 is
/// replaceable, so nostrdb returns revisions newest-first — [`load_board_pref`]
/// takes the first, which is the winning (latest) one.
fn board_pref_filter(author: &Pubkey) -> Filter {
    Filter::new()
        .authors([author.bytes()])
        .kinds([KIND_BOARD_PREF as u64])
        .limit(1)
        .build()
}

/// The [`BoardCoord`] `author` last selected, or `None` if none was ever saved
/// (or the account is watch-only, so nostrdb can't unwrap the PNS envelope). The
/// newest revision wins — the same latest-wins read as the notebook's
/// `load_longform`. The note is stored PNS-wrapped, but nostrdb has already
/// unwrapped it on read (the account key is registered via `Ndb::add_key` at
/// sign-in), so this only ever sees the inner note.
///
/// The content is the selected board's coordinate. A legacy note whose content is
/// a bare slug — written before selection became coordinate-aware — is read as an
/// own board (`owner = author`), so a previously saved preference keeps resolving
/// without a migration.
pub fn load_board_pref(ndb: &Ndb, author: &Pubkey) -> Option<BoardCoord> {
    let txn = Transaction::new(ndb).ok()?;
    let content = ndb
        .query(&txn, &[board_pref_filter(author)], 1)
        .ok()?
        .into_iter()
        .next()
        .map(|r| r.note.content().to_string())
        .filter(|c| !c.is_empty())?;
    Some(match BoardCoord::parse(&content) {
        Some(coord) => coord,
        None => BoardCoord::new(*author.bytes(), content),
    })
}

/// The `created_at` of `author`'s current board-preference note, or 0 if none —
/// the supersede baseline the next save stamps strictly past (see
/// [`crate::store::save_board_pref`]) so a same-second re-save still wins.
pub fn board_pref_created_at(ndb: &Ndb, author: &Pubkey) -> u64 {
    Transaction::new(ndb)
        .ok()
        .and_then(|txn| {
            ndb.query(&txn, &[board_pref_filter(author)], 1)
                .ok()?
                .into_iter()
                .next()
                .map(|r| r.note.created_at())
        })
        .unwrap_or(0)
}

/// The `author`'s current [`BlockerSet`] for the card `blocked`, read straight
/// from `ndb`, or `None` when it has none. Editing a blocker set rebuilds from
/// this *raw* set rather than the folded [`CardView::blocked_by`], which drops
/// edges the fold couldn't resolve (e.g. a cross-board blocker) — reconstructing
/// from the resolved view would silently discard them. The returned
/// [`BlockerSet::created_at`] is the supersede baseline the next write stamps
/// strictly past (see [`crate::store`]). Mirrors [`board_pref_created_at`].
pub fn current_blockers(ndb: &Ndb, author: &Pubkey, blocked: &NoteId) -> Option<BlockerSet> {
    let txn = Transaction::new(ndb).ok()?;
    // Keyed by the `#e` id-tag, not `#d`: the blocked card's id is 64-char hex, so
    // nostrdb stores that `d` value as a 32-byte id rather than a string — a
    // string `#d` filter silently matches nothing (the same trap as `#e`/`#p`).
    // build_blockers carries the same id as an `e` tag, so `.events` reaches it.
    let filter = Filter::new()
        .kinds([KIND_BLOCKERS as u64])
        .authors([author.bytes()])
        .events(std::iter::once(blocked.bytes()))
        .limit(1)
        .build();
    // nostrdb returns replaceable revisions newest-first, so the first match is
    // the winning (latest) set — the same read as [`board_pref_created_at`].
    let note = ndb.query(&txn, &[filter], 1).ok()?.into_iter().next()?.note;
    parse_blockers(&note)
}

/// The `author`'s current [`RelatedSet`] for `card`, read straight from `ndb`, or
/// `None` when it has none. Like [`current_blockers`], edits rebuild from this
/// *raw* stored set rather than the folded [`CardView::related`] — the fold unions
/// both edge directions and drops edges it couldn't resolve (e.g. a cross-board
/// partner), so editing the resolved view would double-count or silently discard
/// them. Keyed by the `#e` id-tag (the card's 64-hex `d` value is stored as a
/// 32-byte id, so a string `#d` filter matches nothing — the same trap as
/// [`current_blockers`]).
pub fn current_related(ndb: &Ndb, author: &Pubkey, card: &NoteId) -> Option<RelatedSet> {
    let txn = Transaction::new(ndb).ok()?;
    let filter = Filter::new()
        .kinds([KIND_RELATED as u64])
        .authors([author.bytes()])
        .events(std::iter::once(card.bytes()))
        .limit(1)
        .build();
    let note = ndb.query(&txn, &[filter], 1).ok()?.into_iter().next()?.note;
    parse_related(&note)
}

// ---------------------------------------------------------------------------
// ndb loading
// ---------------------------------------------------------------------------

/// A filter for every headway event authored by `author`.
///
/// Headway is single-author per board for now, so filtering by author captures
/// the board, its cards and all metadata in one query. Collaborative boards will
/// additionally need `#a`/`#e` filters to pull in other authors' events.
///
/// Deliberately unbounded. This drives [`fold_board`]'s visitor walk, where a
/// `limit` is a cap on notes *visited*, not a page size — so the fold would stop
/// mid-history and silently resolve the board from a truncated event set. Headway
/// events only accumulate (nostrdb never replaces an addressable event, it keeps
/// every revision), so any fixed cap is a bomb on a timer: the board would start
/// losing its oldest cards once an account crossed it, with no error. The board
/// is the whole history or it is wrong.
pub fn headway_filter(author: &Pubkey) -> Filter {
    Filter::new()
        .authors([author.bytes()])
        .kinds(HEADWAY_KINDS.iter().map(|k| *k as u64))
        .build()
}

/// Fold all of `author`'s headway events out of `ndb` into a fresh reducer.
///
/// The reduction runs inside the [`Ndb::fold`] index walk via [`BoardReducer`],
/// so no intermediate event `Vec` is built. nostrdb doesn't replace addressable
/// events, so the placement/board history is walked in full and the reducer
/// resolves the effective state; `query_replaceable_filtered` can narrow the
/// addressable kinds (board, placement) to their latest versions later.
///
/// The caller can keep the returned reducer and feed later arrivals into it with
/// [`reduce_delta`] rather than re-folding the whole history.
#[profiling::function]
pub fn fold_board(ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> Option<BoardReducer> {
    let filters = [headway_filter(author)];
    ndb.fold(txn, &filters, BoardReducer::default(), |mut acc, note| {
        if let Some(event) = parse(&note) {
            acc.ingest(event);
        }
        acc
    })
    .ok()
}

/// The board-anchored half of the shared-board read fan-out: the filters that
/// pull the board *definition* and every member's board-anchored events (issues
/// and placements) regardless of who authored them.
///
/// `headway_filter` scopes by a single author, which only ever sees one member's
/// events. Genuine multi-writer boards ([NIP-SNS](../../../docs)) need to gather
/// every member's contributions, so the read path keys off the board's
/// *coordinate* instead of any one author:
///
/// - `{kinds:[BOARD], authors:[owner], #d:[board_id]}` — the owner-authored board
///   definition (columns, title). Authored only by the owner by construction, so
///   this stays author-scoped.
/// - `{kinds:[ISSUE, PLACEMENT], #a:[board_addr]}` — every member's cards and
///   card placements, which carry the board coordinate in their `a` tag
///   ([`build_issue`], [`build_placement`]) rather than being owner-authored.
///
/// The definition filter is deliberately **unbounded**, for the same reason
/// [`headway_filter`] is: nostrdb keeps every revision of an addressable event,
/// and a `limit` here caps the notes *visited* by the fold rather than paging it.
/// A `limit(1)` would visit only the newest revision at the coordinate — and the
/// newest revision is not necessarily one the caller may trust. [`fold_shared_board`]
/// ingests only team-sealed rumors, so a *plaintext* board definition written at the
/// coordinate afterwards (a stray auto-seed, a front end that doesn't know the board
/// is sealed) would be the only note visited, get dropped by the seal-trust check,
/// and leave the fold with no definition at all — a board that has sealed cards yet
/// renders nothing, permanently. Visiting every revision lets the reducer resolve
/// latest-wins over the revisions the caller actually trusts.
///
/// Returns `None` if `board_addr` isn't a well-formed board coordinate. This is
/// "phase A" of [`fold_shared_board`]; the card ids it surfaces drive the
/// card-anchored "phase B" ([`card_meta_filter`]).
pub fn board_scoped_filters(board_addr: &str) -> Option<Vec<Filter>> {
    let coord = BoardCoord::parse(board_addr)?;
    let def = Filter::new()
        .kinds([KIND_BOARD as u64])
        .authors([&coord.owner])
        .tags([coord.slug.as_str()], 'd')
        .build();
    let anchored = Filter::new()
        .kinds([KIND_ISSUE as u64, KIND_PLACEMENT as u64])
        .tags([board_addr], 'a')
        .limit(5000)
        .build();
    Some(vec![def, anchored])
}

/// The card-anchored half of the shared-board read fan-out: a filter for every
/// member's per-card metadata (subject/label edits, cover notes, relations,
/// sequences, blocker sets, related-to sets and comments), keyed by the `e` tag
/// pointing at each card.
///
/// These overlays name their card by `e` tag and carry no board reference, so
/// they can't be reached by the board coordinate — they're gathered by the set
/// of card ids surfaced in [`board_scoped_filters`]' phase-A walk. "Phase B" of
/// [`fold_shared_board`]. `card_ids` are raw 32-byte note ids; the `#e` tag is an
/// *id* tag, so it's matched with [`Filter::events`] rather than a string tag.
///
/// Comments are gathered separately by [`comment_filter`]: a *reply*'s lowercase
/// `e` points at its parent comment, not the issue, so an `#e:[issue_ids]` filter
/// would miss threaded replies — they're reached by their uppercase root `E`
/// instead.
pub fn card_meta_filter(card_ids: &[[u8; 32]]) -> Filter {
    Filter::new()
        .kinds([
            KIND_LABEL as u64,
            KIND_RELATION as u64,
            KIND_SEQUENCE as u64,
            KIND_COVER_NOTE as u64,
            KIND_BLOCKERS as u64,
            KIND_RELATED as u64,
        ])
        .events(card_ids.iter())
        .limit(5000)
        .build()
}

/// The comment half of the shared-board card-anchored fan-out: a filter for every
/// member's comments (kind 1111) on the given cards, keyed by the NIP-22 root `E`
/// tag rather than the parent `e` tag.
///
/// Every comment — top-level *and* threaded reply — carries the uppercase root
/// `E` = the issue id ([`build_comment`]), whereas the lowercase parent `e` is the
/// issue only for a top-level comment and the *parent comment* for a reply. So
/// matching on `#e:[issue_ids]` (as [`card_meta_filter`] does for other overlays)
/// would silently drop replies; matching on the root `#E` captures the whole
/// comment tree for each card. `E` is an *id* tag, so it's matched with id
/// elements ([`Filter::add_id_element`]), not a string tag — `Filter` has no
/// char-parameterised id-tag helper, so this drives the tag field directly.
pub fn comment_filter(card_ids: &[[u8; 32]]) -> Filter {
    let mut b = Filter::new().kinds([KIND_COMMENT as u64]).limit(5000);
    b.start_tag_field('E').unwrap();
    for id in card_ids {
        b.add_id_element(id).unwrap();
    }
    b.end_field();
    b.build()
}

/// Fold a *shared* board — one written by many members — out of `ndb` into a
/// fresh reducer, gathering every author's events rather than a single author's.
///
/// This is the multi-writer counterpart of [`fold_board`], and the read-path
/// prerequisite that makes concurrent edits observable at all: without it, other
/// members' overlays never reach the reducer, so nothing converges. It runs the
/// two-phase fan-out into one [`BoardReducer`]:
///
/// 1. **Phase A** ([`board_scoped_filters`]): fold the board definition and every
///    member's issues and placements, collecting the card ids off the issues as
///    we walk.
/// 2. **Phase B** ([`card_meta_filter`] + [`comment_filter`]): fold every member's
///    card-anchored overlays for exactly those cards — labels, cover notes,
///    relations and sequences by their `#e` issue tag, and comments (including
///    threaded replies) by their `#E` root tag.
///
/// Both phases feed the *same* reducer; because [`BoardReducer::ingest`] is
/// commutative and idempotent, folding one filter set after another yields the
/// same state as a single combined walk.
///
/// Authority follows *team-key possession*: the walk ingests only team-sealed
/// rumors (see [`team_sealed`]) — those nostrdb unwrapped from a kind-1081
/// envelope sealed under `team_pubkey`, which only a keyholder can produce — and
/// the reducer runs in [`Authority::TeamKey`](reduce::Authority::TeamKey) mode, so any of them may amend any
/// card. This drops plaintext notes forged at the coordinate, and is what lets a
/// non-owner member's edit count. Per-member edit *permissions* (an admin-signed
/// roster) are the separate G6 gate, `headway:headway/purchase-arch-since`.
///
/// `team_pubkeys` are the board channel's team public keys
/// (`nostrdb_net::sns::derive_sns_keys(team_root).team_keypair.pubkey`), the value a
/// kind-1081 envelope is authored by. It is a *set*, not one key, because a board
/// can accumulate more than one channel over its life and its content is then
/// split across them with no way to consolidate: a note is promoted to a sealed
/// rumor only while it is still plaintext, so once sealed it can never be moved
/// into a second channel. Key rotation produces this by design (each epoch is its
/// own root — see *Rotation* in the SNS doc), and a re-seal that ran under a fresh
/// root produces it by accident. Either way the board is the union of every
/// channel the roster holds for its coordinate; folding just one silently
/// truncates it, or — if the channel that happens to be picked lacks the board
/// *definition* — loses the board entirely.
///
/// Returns `None` if `board_addr` isn't a well-formed board coordinate or the
/// index walk fails. A board with no cards yields an empty (phase-A-only) reducer
/// rather than `None`.
#[profiling::function]
pub fn fold_shared_board(
    ndb: &Ndb,
    txn: &Transaction,
    board_addr: &str,
    team_pubkeys: &[Pubkey],
) -> Option<BoardReducer> {
    let phase_a = board_scoped_filters(board_addr)?;
    let teams: Vec<[u8; 32]> = team_pubkeys.iter().map(|k| *k.bytes()).collect();
    let team = teams.as_slice();
    let mut card_ids: Vec<[u8; 32]> = Vec::new();
    let acc = ndb
        .fold(
            txn,
            &phase_a,
            BoardReducer::team_authored(),
            |mut acc, note| {
                if !team_sealed(&note, team) {
                    return acc;
                }
                if note.kind() == KIND_ISSUE {
                    card_ids.push(*note.id());
                }
                if let Some(event) = parse(&note) {
                    acc.ingest(event);
                }
                acc
            },
        )
        .ok()?;

    // No cards means no card-anchored overlays to gather; the `#e`/`#E` filters
    // would be empty, so skip phase B and return the board as-is.
    if card_ids.is_empty() {
        return Some(acc);
    }

    let phase_b = [card_meta_filter(&card_ids), comment_filter(&card_ids)];
    ndb.fold(txn, &phase_b, acc, |mut acc, note| {
        if !team_sealed(&note, team) {
            return acc;
        }
        if let Some(event) = parse(&note) {
            acc.ingest(event);
        }
        acc
    })
    .ok()
}

/// Whether `note` is a rumor nostrdb unwrapped from an SNS kind-1081 envelope
/// sealed under *any* of `team_pubkeys` — i.e. produced by a holder of this board's team
/// key. nostrdb stamps the envelope's ECDH recipient (the team pubkey) into an
/// unwrapped rumor's receiver slot *after* decrypting under the team key, and a
/// plaintext note forged at the board coordinate is not a rumor, so neither leg
/// of this check can be spoofed by a non-keyholder. This is the seal-trust that
/// makes [`Authority::TeamKey`](reduce::Authority::TeamKey) sound.
pub(crate) fn team_sealed(note: &Note, team_pubkeys: &[[u8; 32]]) -> bool {
    note.is_rumor()
        && note
            .rumor_receiver_pubkey()
            .is_some_and(|recv| team_pubkeys.contains(recv))
}

/// Fold a batch of freshly-arrived notes (identified by `keys`) into an existing
/// reducer. Sound because the fold is commutative and idempotent: applying a
/// delta to an up-to-date reducer yields the same state as a full re-fold, so
/// the app can subscribe-then-poll instead of walking the history every frame.
/// Notes that aren't recognised headway events are skipped.
///
/// Returns the keys that couldn't be read under `txn`. A subscription drains a
/// key the instant its note is committed, but a read `txn` is a snapshot fixed
/// at *open* time: a note committed after the caller opened `txn` isn't visible
/// to it yet, so [`get_note_by_key`](Ndb::get_note_by_key) fails even though the
/// note exists. Because polling already removed the key from the subscription
/// inbox, silently skipping it would drop the note until the next full re-seed.
/// Instead we hand those keys back so the caller can retry them on a later
/// advance with that frame's fresher snapshot — the re-fold is idempotent, so a
/// key that turns out to have been visible all along costs nothing to replay.
#[must_use = "keys that couldn't be read must be retried with a fresher txn, not dropped"]
#[profiling::function]
pub fn reduce_delta(
    reducer: &mut BoardReducer,
    ndb: &Ndb,
    txn: &Transaction,
    keys: &[NoteKey],
) -> Vec<NoteKey> {
    let mut deferred = Vec::new();
    for key in keys {
        let Ok(note) = ndb.get_note_by_key(txn, *key) else {
            // Committed after `txn`'s snapshot — retry next advance.
            deferred.push(*key);
            continue;
        };
        if let Some(event) = parse(&note) {
            reducer.ingest(event);
        }
    }
    deferred
}

/// Find the board with `board_id` authored by `author` in an *already-finalized*
/// board set, without re-finalizing. The steady-state inline path finalizes a
/// reducer once per frame (memoized) and then resolves every reference against
/// that one `&[BoardView]` through this, rather than re-walking the reducer per
/// reference (see [`pick_board`], which finalizes on each call).
pub fn find_board<'a>(
    boards: &'a [BoardView],
    author: &Pubkey,
    board_id: &str,
) -> Option<&'a BoardView> {
    boards
        .iter()
        .find(|v| v.id == board_id && &v.author == author.bytes())
}

/// Pick the board with `board_id` authored by `author` out of a reducer's
/// resolved boards, if it exists. Finalizes the reducer; a caller resolving many
/// references against one reducer per frame should finalize once and reuse the
/// result via [`find_board`] instead.
#[profiling::function]
pub fn pick_board(reducer: &BoardReducer, author: &Pubkey, board_id: &str) -> Option<BoardView> {
    find_board(&reducer.finalize(), author, board_id).cloned()
}

/// Pick a single card's *resolved* [`CardView`] (latest subject, labels, cover
/// and placement applied) out of a folded board, by the issue's note id.
/// Searches the live columns and the archived set. `None` if the board or the
/// card within it is absent. Unlike parsing the kind-1621 note directly — which
/// only yields its creation-time snapshot — this reflects later edits.
#[profiling::function]
pub fn pick_card(
    reducer: &BoardReducer,
    author: &Pubkey,
    board_id: &str,
    issue_id: &[u8; 32],
) -> Option<CardView> {
    card_in_board(&pick_board(reducer, author, board_id)?, issue_id)
}

/// Pick a card's resolved [`CardView`] out of an *already-finalized* board,
/// searching its live columns then its archived set. The re-finalize-free core of
/// [`pick_card`]: the inline render path finalizes once per frame and resolves
/// each referenced card through this.
pub fn card_in_board(view: &BoardView, issue_id: &[u8; 32]) -> Option<CardView> {
    let want = NoteId::new(*issue_id);
    view.columns
        .iter()
        .flat_map(|col| col.cards.iter())
        .chain(view.archived.iter().map(|a| &a.card))
        .find(|card| card.id == want)
        .cloned()
}

/// A card's position among a board's live columns: which column it sits in and
/// how many columns there are. Enough to derive a positional (Linear-style)
/// status indicator, which maps the first column to backlog and the last to done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColumnPos {
    /// Zero-based index of the card's column in board order.
    pub index: usize,
    /// Total number of live columns on the board.
    pub count: usize,
}

/// A card resolved for inline display: its [`CardView`] plus the live
/// [`ColumnPos`] used to show a status indicator. See [`pick_card_with_column`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedCard {
    /// The card's resolved state (latest subject, labels and cover applied).
    pub card: CardView,
    /// The card's live column position, or `None` when it is archived (not in a
    /// live column).
    pub column: Option<ColumnPos>,
}

/// Like [`pick_card`], but also resolves the card's live [`ColumnPos`] so an
/// inline reference can show a status indicator. Returns `None` when the board
/// or card is absent.
#[profiling::function]
pub fn pick_card_with_column(
    reducer: &BoardReducer,
    author: &Pubkey,
    board_id: &str,
    issue_id: &[u8; 32],
) -> Option<ResolvedCard> {
    card_with_column_in_board(&pick_board(reducer, author, board_id)?, issue_id)
}

/// Resolve a card *and* its live [`ColumnPos`] out of an *already-finalized*
/// board. The re-finalize-free core of [`pick_card_with_column`]: the inline chip
/// render path finalizes once per frame and resolves each referenced card through
/// this.
pub fn card_with_column_in_board(view: &BoardView, issue_id: &[u8; 32]) -> Option<ResolvedCard> {
    let want = NoteId::new(*issue_id);
    let count = view.columns.len();
    for (index, col) in view.columns.iter().enumerate() {
        if let Some(card) = col.cards.iter().find(|c| c.id == want) {
            return Some(ResolvedCard {
                card: card.clone(),
                column: Some(ColumnPos { index, count }),
            });
        }
    }
    view.archived
        .iter()
        .map(|a| &a.card)
        .find(|card| card.id == want)
        .cloned()
        .map(|card| ResolvedCard { card, column: None })
}

/// A card resolved for inline display together with the board it currently lives
/// on. For a card that was moved across boards this is the *destination* board —
/// where [`finalize`](BoardReducer::finalize) actually places it — not the origin
/// board recorded in the card's `a` tag. See [`locate_card`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocatedCard {
    /// The board the card is live on — the [`BoardView`] this resolution came
    /// from, and the board a click on the card should open.
    pub board_id: String,
    /// The card's resolved state (latest subject, labels and cover applied).
    pub card: CardView,
    /// The card's live [`ColumnPos`], or `None` when it is archived (off the live
    /// columns).
    pub column: Option<ColumnPos>,
}

/// Resolve a card for inline display across *every* board `author` owns, rather
/// than assuming the board recorded in the card's `a` tag.
///
/// A card's `a` tag records its *origin* board, but a cross-board move deletes it
/// there and places it on the destination — membership follows the live placement
/// ([`finalize`](BoardReducer::finalize) is placement-driven). Resolving against
/// the stale `a`-tag board would find the card deleted (or absent) and render an
/// "invalid" chip that opens the wrong board, so we scan the finalized boards and
/// return the card where it is actually shown.
///
/// A live column placement is preferred over an archived one, and among live
/// boards the newest placement (by [`CardView::placed_at`]) wins — a card is
/// normally placed on exactly one board, so a move is unambiguous; a genuine
/// multi-board placement resolves to its most-recently-touched board. `None` when
/// the card is on no board of this author (deleted everywhere, or its board isn't
/// folded — the caller falls back to the card's creation-time snapshot).
pub fn locate_card(
    reducer: &BoardReducer,
    author: &Pubkey,
    issue_id: &[u8; 32],
) -> Option<LocatedCard> {
    locate_card_in_boards(&reducer.finalize(), author, issue_id)
}

/// Resolve a card across an *already-finalized* board set, preferring a live
/// column placement over an archived one and the newest placement among live
/// boards. The re-finalize-free core of [`locate_card`]: the inline render path
/// finalizes once per frame (memoized) and locates each referenced card through
/// this, mirroring [`card_with_column_in_board`]'s split from [`pick_card_with_column`].
pub fn locate_card_in_boards(
    boards: &[BoardView],
    author: &Pubkey,
    issue_id: &[u8; 32],
) -> Option<LocatedCard> {
    let want = NoteId::new(*issue_id);
    boards
        .iter()
        .filter(|board| &board.author == author.bytes())
        .filter_map(|board| {
            let count = board.columns.len();
            // A live column hit resolves to a status; prefer it over archived.
            for (index, col) in board.columns.iter().enumerate() {
                if let Some(card) = col.cards.iter().find(|c| c.id == want) {
                    return Some(LocatedCard {
                        board_id: board.id.clone(),
                        card: card.clone(),
                        column: Some(ColumnPos { index, count }),
                    });
                }
            }
            board
                .archived
                .iter()
                .map(|a| &a.card)
                .find(|c| c.id == want)
                .cloned()
                .map(|card| LocatedCard {
                    board_id: board.id.clone(),
                    card,
                    column: None,
                })
        })
        .max_by(|a, b| {
            a.column
                .is_some()
                .cmp(&b.column.is_some())
                .then(a.card.placed_at.cmp(&b.card.placed_at))
        })
}

/// Fold `author`'s headway events out of `ndb` and reduce them into the board
/// with the given `board_id`, if it exists. A one-shot [`fold_board`] +
/// [`pick_board`] for callers that don't keep the reducer around.
#[profiling::function]
pub fn load_board(
    ndb: &Ndb,
    txn: &Transaction,
    author: &Pubkey,
    board_id: &str,
) -> Option<BoardView> {
    pick_board(&fold_board(ndb, txn, author)?, author, board_id)
}

/// Fold a joined shared board by its `board_addr` coordinate
/// (`30619:<owner>:<slug>`) and return its single [`BoardView`], gathering every
/// member's events. The multi-writer analogue of [`load_board`]: a one-shot
/// [`fold_shared_board`] + finalize for callers that don't keep the reducer
/// around (`BoardCache::shared_board` is the memoized in-app path). `None` until
/// the board's definition has folded in. `team_pubkey` is the board channel's
/// team key — see [`fold_shared_board`].
#[profiling::function]
pub fn load_shared_board(
    ndb: &Ndb,
    txn: &Transaction,
    board_addr: &str,
    team_pubkeys: &[Pubkey],
) -> Option<BoardView> {
    // fold_shared_board folds a single coordinate, so its finalize yields the one
    // board (empty until the board definition has arrived).
    fold_shared_board(ndb, txn, board_addr, team_pubkeys)?
        .finalize()
        .into_iter()
        .next()
}

/// Every card on `view` in a stable order — the live columns' cards, in column
/// then rank order, followed by the archived cards. Shared by callers that
/// resolve a card by re-encoding each one (word ids, hex prefixes).
pub fn all_cards(view: &BoardView) -> impl Iterator<Item = &CardView> {
    view.columns
        .iter()
        .flat_map(|c| c.cards.iter())
        .chain(view.archived.iter().map(|a| &a.card))
}

/// Resolve a card on `view` by its three-word id (`word-word-word`, *without*
/// the board slug prefix) by re-encoding every card and matching — exactly how a
/// git short hash resolves, and how the CLI's `resolve_card` matches word ids.
/// Archived cards are included. `None` if no card encodes to `words`.
///
/// Shared by the CLI and the inline `headway` [reference
/// parser](../../notedeck_headway) so both agree on what a word id resolves to.
#[profiling::function]
pub fn resolve_card_by_wordid(view: &BoardView, words: &str) -> Option<NoteId> {
    all_cards(view)
        .find(|c| crate::wordid::encode(c.id.bytes()) == words)
        .map(|c| c.id)
}

/// Resolve a card ref on `view` to its note id, accepting (in order): a full
/// 64-char hex id; a reference `headway:<board>/<word-id>` or its scheme-less
/// `<board>/<word-id>` shorthand; a **bare word-id or any unique prefix of one**
/// (matched against the already-routed board, exactly like a git short hash); or
/// a unique hex prefix. The word-id is matched against every card on the board,
/// archived ones included; the board segment is informational here (the caller
/// already routed to `view`).
///
/// A bare word-id resolving here is *not* a self-routing reference — the board is
/// already selected — so it stays consistent with [`crate::wordid::parse_ref`]
/// (which still rejects bare word-ids for routing / inline parsing). It just
/// means an agent that types `headway --board X show slush-derive-answer`, or the
/// prefix `slush-derive` (or even `slush`), reaches the card without having to
/// repeat the board segment it already passed. Only an all-`0-9a-f` selector is
/// read as a hex prefix; anything with a non-hex letter or a `-` is a word-id.
///
/// This is the single card-addressing entry point shared by the CLI and the
/// in-app agent tools ([`notedeck_headway`](../../notedeck_headway)), so both
/// frontends resolve a user's card ref identically.
pub fn resolve_card(view: &BoardView, sel: &str) -> Result<NoteId, String> {
    if let Ok(id) = NoteId::from_hex(sel) {
        return Ok(id);
    }
    let sel = sel.to_lowercase();

    // Strip the board segment off a `headway:<board>/<word-id>` reference (or its
    // scheme-less shorthand) if present; otherwise treat the whole selector as a
    // bare word-id / hex prefix. The board segment self-routed the fold already,
    // so only the word-id half matters here.
    let words = crate::wordid::parse_ref(&sel)
        .map(|(_board, words)| words)
        .unwrap_or(sel.as_str());

    // Anything that isn't purely hex digits is a word-id selector, not a hex
    // prefix (a real hex prefix can only contain `0-9a-f`; a word-id carries
    // BIP-39 letters and `-`). Match it against the board like a git short hash:
    // an exact word-id or any unique prefix resolves, and a miss suggests the
    // near cards. Re-encoding each card is how word ids resolve everywhere (see
    // [`resolve_card_by_wordid`]).
    if !words.is_empty() && !words.bytes().all(|b| b.is_ascii_hexdigit()) {
        let mut hits = all_cards(view).filter(|c| wordid::encode(c.id.bytes()).starts_with(words));
        return match (hits.next(), hits.next()) {
            (Some(c), None) => Ok(c.id),
            (Some(_), Some(_)) => Err(ambiguous_wordid_err(view, words)),
            _ => Err(no_wordid_match_err(view, words)),
        };
    }

    let mut hits = all_cards(view).filter(|c| c.id.hex().starts_with(&sel));
    match (hits.next(), hits.next()) {
        (Some(c), None) => Ok(c.id),
        (Some(_), Some(_)) => Err(format!("ambiguous card prefix '{sel}'")),
        _ => Err(format!("no card matching '{sel}'")),
    }
}

/// Word ids on `view` that start with `words`, sorted for a stable message.
/// Only used off the error path, so the allocation is fine.
fn wordid_prefix_matches(view: &BoardView, words: &str) -> Vec<String> {
    let mut matches: Vec<String> = all_cards(view)
        .map(|c| wordid::encode(c.id.bytes()))
        .filter(|w| w.starts_with(words))
        .collect();
    matches.sort();
    matches.dedup();
    matches
}

/// Error for a word-id prefix that matches more than one card, listing the
/// candidates so the caller can lengthen the prefix to disambiguate.
fn ambiguous_wordid_err(view: &BoardView, words: &str) -> String {
    format!(
        "ambiguous word-id '{words}'; matches: {}",
        wordid_prefix_matches(view, words).join(", ")
    )
}

/// Error for a word-id that matches no card. Suggests cards sharing the query's
/// leading word so a near-miss guess (a typo in the last word, a stale id) still
/// points at the real card instead of a bare "no card matching".
fn no_wordid_match_err(view: &BoardView, words: &str) -> String {
    let lead = words.split(crate::wordid::SEP).next().unwrap_or(words);
    let mut near: Vec<String> = all_cards(view)
        .map(|c| wordid::encode(c.id.bytes()))
        .filter(|w| w.split(crate::wordid::SEP).next() == Some(lead))
        .collect();
    near.sort();
    near.dedup();
    if near.is_empty() {
        format!("no card matching word-id '{words}'; run `show` to list the board's cards")
    } else {
        format!(
            "no card matching word-id '{words}'; did you mean: {}",
            near.join(", ")
        )
    }
}

// ---------------------------------------------------------------------------
// Fractional ranking
// ---------------------------------------------------------------------------

/// Smallest rank digit value below `'a'` and above `'z'` used as open bounds.
const RANK_LOW: u8 = b'a' - 1;

const RANK_HIGH: u8 = b'z' + 1;

/// Produce a rank string that sorts strictly between `left` and `right` (each an
/// optional existing rank). `None` means "open" — i.e. `rank_between(None, None)`
/// is the first rank, `rank_between(Some(last), None)` appends after `last`, and
/// `rank_between(None, Some(first))` prepends before `first`.
///
/// Ranks are lowercase `a`–`z` strings compared lexicographically. Appending and
/// inserting-between are unbounded (ranks just grow in length), but prepending
/// repeatedly walks toward `"a"` and nothing sorts before `"a"`; exhausting the
/// low end requires a rank rebalance (future work). New boards seed from the
/// midpoint to keep headroom on both sides.
pub fn rank_between(left: Option<&str>, right: Option<&str>) -> String {
    let l = left.unwrap_or("").as_bytes();
    let r = right.unwrap_or("").as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    let mut right_open = false;

    loop {
        let lc = l.get(i).copied().unwrap_or(RANK_LOW);
        let rc = if right_open {
            RANK_HIGH
        } else {
            r.get(i).copied().unwrap_or(RANK_HIGH)
        };

        let mid = (lc + rc) / 2;
        if mid != lc {
            out.push(mid);
            return String::from_utf8(out).expect("ascii rank");
        }

        // lc and rc are adjacent (or equal): keep this digit and descend. Once
        // we've committed a digit equal to lc while rc == lc + 1, every deeper
        // digit is already < right, so the right bound is released.
        out.push(if lc == RANK_LOW { b'a' } else { lc });
        if rc == lc + 1 {
            right_open = true;
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::parse::tests::note_id;
    use crate::test_config;
    use nostrdb::NoteBuilder;
    use nostrdb_net::FullKeypair;

    #[test]
    fn rank_between_appends_in_increasing_order() {
        let mut last = rank_between(None, None);
        for _ in 0..50 {
            let next = rank_between(Some(&last), None);
            assert!(next > last, "{next:?} should be > {last:?}");
            last = next;
        }
    }

    #[test]
    fn rank_between_prepends_in_decreasing_order() {
        // Prepending repeatedly walks toward the "a" floor; a few levels are
        // always available (a real rebalance is needed to go below "a", which
        // is tracked as future work — see `rank_between` docs).
        let mut first = rank_between(None, None);
        for _ in 0..3 {
            let prev = rank_between(None, Some(&first));
            assert!(prev < first, "{prev:?} should be < {first:?}");
            assert!(prev.bytes().all(|b| b.is_ascii_lowercase()));
            first = prev;
        }
    }

    #[test]
    fn rank_between_inserts_strictly_between() {
        let a = rank_between(None, None);
        let b = rank_between(Some(&a), None);
        for _ in 0..50 {
            let mid = rank_between(Some(&a), Some(&b));
            assert!(
                mid > a && mid < b,
                "{mid:?} not strictly between {a:?},{b:?}"
            );
        }
        // Adjacent ranks still admit an in-between value by growing length.
        let lo = "m".to_string();
        let hi = "n".to_string();
        let mid = rank_between(Some(&lo), Some(&hi));
        assert!(mid > lo && mid < hi, "{mid:?} not between {lo:?},{hi:?}");
    }

    /// End-to-end through a real nostrdb: build + sign events, ingest them, then
    /// fold them back out with [`load_board`] and check the board reconstructs
    /// (including a subject rename overriding the issue's original subject).
    #[test]
    fn load_board_roundtrips_through_ndb() {
        use nostrdb::{IngestMetadata, Ndb, Transaction};
        use std::time::{Duration, Instant};

        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();
        let kp = FullKeypair::generate();
        let addr = board_address(&kp.pubkey, "headway");

        let ingest = |b: NoteBuilder| -> NoteId {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            let id = NoteId::new(*note.id());
            let json = nostrdb_net::ClientMessage::event(&note)
                .unwrap()
                .to_json()
                .unwrap();
            ndb.process_event_with(&json, IngestMetadata::new().client(true))
                .unwrap();
            id
        };

        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];
        ingest(build_board("headway", "Headway", "", &cols));
        let a = ingest(build_issue(&addr, "Card A", ""));
        let b = ingest(build_issue(&addr, "Card B", ""));
        ingest(build_placement("headway", &addr, &a, "todo", "g"));
        ingest(build_placement("headway", &addr, &b, "done", "m"));
        ingest(build_subject_edit(&a, "Card A (renamed)"));

        // ndb ingests on a writer thread; poll until the board materialises.
        let deadline = Instant::now() + Duration::from_secs(5);
        let view = loop {
            let txn = Transaction::new(&ndb).unwrap();
            if let Some(view) = load_board(&ndb, &txn, &kp.pubkey, "headway")
                && view.columns[0].cards.len() == 1
                && view.columns[1].cards.len() == 1
            {
                break view;
            }
            assert!(
                Instant::now() < deadline,
                "board did not materialise in ndb"
            );
            std::thread::sleep(Duration::from_millis(20));
        };

        assert_eq!(view.columns.len(), 2);
        assert_eq!(view.columns[0].name, "Todo");
        assert_eq!(view.columns[0].cards[0].title, "Card A (renamed)");
        assert_eq!(view.columns[1].cards[0].title, "Card B");
    }

    #[test]
    fn current_blockers_reads_latest_set_from_ndb() {
        use nostrdb::{IngestMetadata, Ndb};
        use std::time::{Duration, Instant};

        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();
        let kp = FullKeypair::generate();

        let ingest = |b: NoteBuilder| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            let json = nostrdb_net::ClientMessage::event(&note)
                .unwrap()
                .to_json()
                .unwrap();
            ndb.process_event_with(&json, IngestMetadata::new().client(true))
                .unwrap();
        };

        let a = NoteId::new([1; 32]);
        let b = NoteId::new([2; 32]);
        let c = NoteId::new([3; 32]);
        ingest(build_blockers(&a, &[b]).created_at(1_000));
        ingest(build_blockers(&a, &[b, c]).created_at(2_000));

        // Poll until the newer set is queryable (ndb ingests on a writer thread).
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(set) = current_blockers(&ndb, &kp.pubkey, &a)
                && set.created_at == 2_000
            {
                assert_eq!(set.blockers, vec![*b.bytes(), *c.bytes()]);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "current_blockers never saw the latest set"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A board written by two different members converges through
    /// [`fold_shared_board`], and authority follows team-key possession: every
    /// edit is a team-sealed rumor, so the owner-authored card, a *different*
    /// member's own card (with that member's placement, subject edit and
    /// comments), *and* that member's edit of the **owner's** card all land in one
    /// board. The single-author [`fold_board`] misses the second member entirely,
    /// which is the gap this closes.
    #[test]
    fn fold_shared_board_gathers_and_trusts_all_team_members() {
        use crate::store::{self, NoPublish, Signer, SnsChannel};
        use nostrdb::{Ndb, Transaction};
        use std::time::{Duration, Instant};

        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();

        let owner = FullKeypair::generate();
        let member = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "headway");

        // A shared board is a team channel: every edit is sealed into a kind-1081
        // envelope under the team key, and nostrdb auto-unwraps it back to a rumor
        // once the team_root is registered. Distinctive bytes so a stray all-zero
        // root can't accidentally match.
        let mut root = [0u8; 32];
        root[0] = 0x11;
        root[31] = 0x42;
        let channel = SnsChannel {
            keys: nostrdb_net::sns::derive_sns_keys(&root).expect("derive sns keys"),
        };
        assert!(ndb.add_team_root(&root));

        // Seal a builder into the channel signed by an arbitrary member, returning
        // the (stable) rumor id nostrdb recomputes on unwrap.
        let seal = |b: NoteBuilder, kp: &FullKeypair| -> NoteId {
            store::ingest_signed(
                &ndb,
                b,
                &Signer::shared(&kp.secret_key.secret_bytes(), &channel),
                &mut NoPublish,
            )
            .expect("sealed ingest")
        };

        let cols = vec![
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];
        // Owner defines the board and authors one card.
        seal(build_board("headway", "Headway", "", &cols), &owner);
        let a = seal(build_issue(&addr, "Owner card", ""), &owner);
        seal(build_placement("headway", &addr, &a, "todo", "g"), &owner);

        // A *different* member authors their own card, places it, and renames it.
        let b = seal(build_issue(&addr, "Member card", ""), &member);
        seal(build_placement("headway", &addr, &b, "done", "m"), &member);
        seal(build_subject_edit(&b, "Member card (renamed)"), &member);

        // ...then comments on it, including a threaded reply. A reply's lowercase
        // `e` points at its parent comment (not the issue), so it's only reachable
        // via the `#E` root tag — the case comment_filter exists to cover.
        let c1 = seal(
            build_comment(&b, &member.pubkey, None, "member comment"),
            &member,
        );
        seal(
            build_comment(
                &b,
                &member.pubkey,
                Some((&c1, &member.pubkey)),
                "member reply",
            ),
            &member,
        );

        // The member edits the OWNER's card. Under team-key authority this counts
        // (any keyholder may edit any card); the old author-or-owner gate dropped
        // it. This is the pre-roster authority — see `Authority::TeamKey`.
        seal(
            build_subject_edit(&a, "Owner card (member-edited)"),
            &member,
        );

        let team_pubkey = &channel.keys.team_keypair.pubkey;
        let deadline = Instant::now() + Duration::from_secs(5);
        let view = loop {
            let txn = Transaction::new(&ndb).unwrap();
            if let Some(reducer) = fold_shared_board(&ndb, &txn, &addr, std::slice::from_ref(team_pubkey))
                && let Some(view) = pick_board(&reducer, &owner.pubkey, "headway")
                && view.columns[0].cards.len() == 1
                && view.columns[1].cards.len() == 1
                // Wait on the members' edits and comments (unwrapped last) so the
                // assertions don't race them into view.
                && view.columns[0].cards[0].title == "Owner card (member-edited)"
                && view.columns[1].cards[0].title == "Member card (renamed)"
                && view.columns[1].cards[0].comments.len() == 2
            {
                break view;
            }
            assert!(
                Instant::now() < deadline,
                "shared board did not materialise in ndb"
            );
            std::thread::sleep(Duration::from_millis(20));
        };

        // Both members' cards converge into one board, the member's own subject
        // edit applied — and the member's edit of the *owner's* card counts too.
        assert_eq!(view.columns[0].cards[0].title, "Owner card (member-edited)");
        assert_eq!(view.columns[1].cards[0].title, "Member card (renamed)");

        // The whole comment thread folds in: the top-level comment and the reply
        // threaded under it. The reply is the regression guard — matching card
        // comments on `#e:[issue_ids]` alone would drop it. Find by body rather
        // than index: the two are stamped in the same wall-clock second, so their
        // (created_at, id) sort order isn't insertion order.
        let comments = &view.columns[1].cards[0].comments;
        let root = comments
            .iter()
            .find(|c| c.body == "member comment")
            .expect("top-level comment folded in");
        let reply = comments
            .iter()
            .find(|c| c.body == "member reply")
            .expect("threaded reply folded in");
        assert_eq!(root.parent, None);
        assert_eq!(reply.parent, Some(c1));

        // The single-author fold sees only the owner's card, still titled as the
        // owner left it: fold_board gathers only the owner's own events, so the
        // member's cross-edit is invisible to it. Exactly the multi-writer gap
        // fold_shared_board closes.
        let txn = Transaction::new(&ndb).unwrap();
        let owner_only = fold_board(&ndb, &txn, &owner.pubkey)
            .and_then(|r| pick_board(&r, &owner.pubkey, "headway"))
            .unwrap();
        assert_eq!(owner_only.columns[0].cards.len(), 1);
        assert_eq!(owner_only.columns[1].cards.len(), 0);
        assert_eq!(owner_only.columns[0].cards[0].title, "Owner card");
    }

    /// A *plaintext* board definition written at a sealed board's coordinate after
    /// the sealed one must not hide it.
    ///
    /// nostrdb keeps every revision of an addressable event and serves the newest
    /// first, so the newest definition at a coordinate is simply whatever was
    /// written last — while [`fold_shared_board`] trusts only team-sealed rumors.
    /// With the definition filter capped at `limit(1)` the fold visited *only* that
    /// newest revision, dropped it as unsealed, and yielded a board with no
    /// definition — `None` forever, on a board whose cards were all sealed and
    /// present. Sighted live as a shared board that rendered nothing behind the
    /// plaintext "Headway" definitions a since-retired auto-seed left at its
    /// coordinate.
    #[test]
    fn shared_fold_sees_sealed_definition_behind_a_later_plaintext_one() {
        use crate::store::{self, NoPublish, Signer, SnsChannel};
        use nostrdb::{Ndb, Transaction};
        use std::time::{Duration, Instant};

        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();

        let owner = FullKeypair::generate();
        let secret = owner.secret_key.secret_bytes();
        let addr = board_address(&owner.pubkey, "headway");

        // Distinctive bytes so a stray all-zero root can't accidentally match.
        let mut root = [0u8; 32];
        root[0] = 0x11;
        root[31] = 0x43;
        let channel = SnsChannel {
            keys: nostrdb_net::sns::derive_sns_keys(&root).expect("derive sns keys"),
        };
        assert!(ndb.add_team_root(&root));

        let cols = vec![ColumnDef::new("todo", "Todo")];
        let seal = |b: NoteBuilder| -> NoteId {
            store::ingest_signed(&ndb, b, &Signer::shared(&secret, &channel), &mut NoPublish)
                .expect("sealed ingest")
        };

        // The real board: definition and one card, all sealed into the channel.
        seal(build_board("headway", "Team Board", "", &cols).created_at(1_000));
        let card = seal(build_issue(&addr, "Sealed card", "").created_at(1_000));
        seal(build_placement("headway", &addr, &card, "todo", "g").created_at(1_000));

        // A plaintext writer then seeds its own definition at the same coordinate,
        // stamped *later* — so it is the revision the coordinate resolves to.
        store::ingest(
            &ndb,
            build_board("headway", "Squatter", "", &cols).created_at(2_000),
            &secret,
            &mut NoPublish,
        )
        .expect("plaintext ingest");

        let team_pubkey = &channel.keys.team_keypair.pubkey;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let txn = Transaction::new(&ndb).unwrap();
            // Require the plaintext revision to have actually won latest-wins first,
            // or the shared fold could pass for the wrong reason (the squatter not
            // yet ingested). The single-author fold has no seal-trust check, so it
            // reports which revision the coordinate resolves to.
            let squatted = fold_board(&ndb, &txn, &owner.pubkey)
                .and_then(|r| pick_board(&r, &owner.pubkey, "headway"))
                .is_some_and(|v| v.title == "Squatter");
            if squatted
                && let Some(view) =
                    load_shared_board(&ndb, &txn, &addr, std::slice::from_ref(team_pubkey))
                && view.columns[0].cards.len() == 1
            {
                // The sealed definition still folds: the plaintext one is dropped by
                // the seal-trust check instead of taking the board down with it.
                assert_eq!(view.title, "Team Board");
                assert_eq!(view.columns[0].cards[0].title, "Sealed card");
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the sealed board never folded behind the later plaintext definition"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn current_related_reads_latest_set_from_ndb() {
        use nostrdb::{IngestMetadata, Ndb};
        use std::time::{Duration, Instant};

        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();
        let kp = FullKeypair::generate();

        let ingest = |b: NoteBuilder| {
            let note = b.sign(&kp.secret_key.secret_bytes()).build().unwrap();
            let json = nostrdb_net::ClientMessage::event(&note)
                .unwrap()
                .to_json()
                .unwrap();
            ndb.process_event_with(&json, IngestMetadata::new().client(true))
                .unwrap();
        };

        let a = NoteId::new([1; 32]);
        let b = NoteId::new([2; 32]);
        let c = NoteId::new([3; 32]);
        ingest(build_related(&a, &[b]).created_at(1_000));
        ingest(build_related(&a, &[b, c]).created_at(2_000));

        // Poll until the newer set is queryable (ndb ingests on a writer thread).
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(set) = current_related(&ndb, &kp.pubkey, &a)
                && set.created_at == 2_000
            {
                assert_eq!(set.related, vec![*b.bytes(), *c.bytes()]);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "current_related never saw the latest set"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// [`pick_card`] resolves a single card to its *current* state — the latest
    /// subject and label edits applied — not the issue's creation-time snapshot,
    /// and returns `None` for an unknown card id.
    #[test]
    fn pick_card_resolves_current_state() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![ColumnDef::new("todo", "Todo")];

        let parse_owned = |b: NoteBuilder| {
            let note = b.sign(&owner.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let i1 = note_id(&owner, build_issue(&addr, "Original", "body"));
        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols)),
            parse_owned(build_issue(&addr, "Original", "body")),
            parse_owned(build_placement("b1", &addr, &i1, "todo", "m")),
            parse_owned(build_subject_edit(&i1, "Renamed")),
            parse_owned(build_labels(&i1, &["bug".to_string()])),
        ];

        let mut reducer = BoardReducer::default();
        for event in &events {
            reducer.ingest(event.clone());
        }

        let card = pick_card(&reducer, &owner.pubkey, "b1", i1.bytes()).unwrap();
        assert_eq!(card.title, "Renamed");
        assert_eq!(card.labels, vec!["bug".to_string()]);

        // Unknown card id -> None.
        assert!(pick_card(&reducer, &owner.pubkey, "b1", &[0u8; 32]).is_none());
    }

    #[test]
    fn pick_card_with_column_resolves_position() {
        let owner = FullKeypair::generate();
        let addr = board_address(&owner.pubkey, "b1");
        let cols = vec![
            ColumnDef::new("backlog", "Backlog"),
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("done", "Done"),
        ];

        let parse_owned = |b: NoteBuilder| {
            let note = b.sign(&owner.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        let i1 = note_id(&owner, build_issue(&addr, "In the middle", "body"));
        let events = vec![
            parse_owned(build_board("b1", "Board", "", &cols)),
            parse_owned(build_issue(&addr, "In the middle", "body")),
            // Placed in the second of three columns.
            parse_owned(build_placement("b1", &addr, &i1, "todo", "m")),
        ];

        let mut reducer = BoardReducer::default();
        for event in &events {
            reducer.ingest(event.clone());
        }

        let resolved = pick_card_with_column(&reducer, &owner.pubkey, "b1", i1.bytes()).unwrap();
        assert_eq!(resolved.card.title, "In the middle");
        assert_eq!(resolved.column, Some(ColumnPos { index: 1, count: 3 }));

        // Unknown card id -> None.
        assert!(pick_card_with_column(&reducer, &owner.pubkey, "b1", &[0u8; 32]).is_none());
    }

    /// A card moved across boards keeps its origin board in its `a` tag but lives
    /// on the destination via its placement. [`locate_card`] resolves it on the
    /// destination (where it's actually shown), not the stale origin — the board
    /// an inline chip must read for its status and a click must open.
    #[test]
    fn locate_card_resolves_cross_board_move() {
        let owner = FullKeypair::generate();
        // The card's `a` tag anchors it to "notedeck" (its origin board).
        let origin = board_address(&owner.pubkey, "notedeck");
        let dest = board_address(&owner.pubkey, "dave");
        let cols = vec![
            ColumnDef::new("backlog", "Backlog"),
            ColumnDef::new("todo", "Todo"),
            ColumnDef::new("in-progress", "In Progress"),
            ColumnDef::new("in-review", "In Review"),
            ColumnDef::new("done", "Done"),
        ];

        let parse_owned = |b: NoteBuilder| {
            let note = b.sign(&owner.secret_key.secret_bytes()).build().unwrap();
            parse(&note).unwrap()
        };

        // Moved card: created on notedeck, deleted there, placed live on dave.
        let moved = note_id(&owner, build_issue(&origin, "Moved", "body"));
        // Orphan: anchored to notedeck by its `a` tag, never placed anywhere.
        let orphan = note_id(&owner, build_issue(&origin, "Orphan", "body"));

        let events = vec![
            parse_owned(build_board("notedeck", "Notedeck", "", &cols)),
            parse_owned(build_board("dave", "Dave", "", &cols)),
            parse_owned(build_issue(&origin, "Moved", "body")),
            parse_owned(build_issue(&origin, "Orphan", "body")),
            // Origin history: placed then deleted (the cross-board move's origin half).
            parse_owned(
                build_placement("notedeck", &origin, &moved, "todo", "m").created_at(1_000),
            ),
            parse_owned(
                build_placement("notedeck", &origin, &moved, COL_DELETED, "m").created_at(2_000),
            ),
            // Destination: live in In Progress (index 2 of 5).
            parse_owned(
                build_placement("dave", &dest, &moved, "in-progress", "m").created_at(3_000),
            ),
        ];

        let mut reducer = BoardReducer::default();
        for event in &events {
            reducer.ingest(event.clone());
        }

        // The moved card resolves on dave (the live placement), not its `a`-tag
        // origin — with its real column position.
        let located = locate_card(&reducer, &owner.pubkey, moved.bytes()).unwrap();
        assert_eq!(located.board_id, "dave");
        assert_eq!(located.card.title, "Moved");
        assert_eq!(located.column, Some(ColumnPos { index: 2, count: 5 }));

        // The bug this guards: the board-scoped resolver keyed on the origin `a`-tag
        // board finds the card deleted there and returns nothing.
        assert!(
            pick_card_with_column(&reducer, &owner.pubkey, "notedeck", moved.bytes()).is_none(),
            "card is deleted on its origin board"
        );

        // An orphan (no placement anywhere) still resolves on its origin board via
        // the finalize fallback — first column.
        let located = locate_card(&reducer, &owner.pubkey, orphan.bytes()).unwrap();
        assert_eq!(located.board_id, "notedeck");
        assert_eq!(located.column, Some(ColumnPos { index: 0, count: 5 }));

        // Unknown card id -> None.
        assert!(locate_card(&reducer, &owner.pubkey, &[0u8; 32]).is_none());
    }

    /// A minimal live card carrying only the id resolution keys off. Every other
    /// field is a harmless default so the resolver tests can name a board full of
    /// cards without spelling out the whole [`CardView`].
    fn view_card(id: NoteId) -> CardView {
        CardView {
            id,
            author: [0; 32],
            title: String::new(),
            description: String::new(),
            labels: vec![],
            priority: Priority::None,
            due: None,
            estimate: None,
            rank: "m".into(),
            seq: None,
            placed_at: 0,
            created_at: 0,
            updated_at: 0,
            comments: vec![],
            activity: vec![],
            parent: None,
            subissues: vec![],
            blocked_by: vec![],
            blocks: vec![],
            related: vec![],
        }
    }

    /// A board named `commerce` holding `ids` in a single `todo` column.
    fn view_with_cards(ids: &[NoteId]) -> BoardView {
        BoardView {
            id: "commerce".into(),
            author: [0; 32],
            title: "Commerce".into(),
            description: String::new(),
            created_at: 0,
            columns: vec![ColumnView {
                id: "todo".into(),
                name: "Todo".into(),
                terminal: false,
                cards: ids.iter().copied().map(view_card).collect(),
            }],
            archived: vec![],
        }
    }

    /// An id whose first four bytes are `seed` (the rest zero), for scanning the
    /// word-id space deterministically without `Date::now`/random.
    fn seeded_id(seed: u32) -> NoteId {
        let mut b = [0u8; 32];
        b[..4].copy_from_slice(&seed.to_be_bytes());
        NoteId::new(b)
    }

    /// Two distinct ids whose word-ids share a leading word — found by pigeonhole
    /// over the 2048-word first slot, so it always terminates. Used to force the
    /// ambiguous-prefix and near-miss error paths.
    fn colliding_leading_word() -> (NoteId, NoteId, String) {
        let mut seen: std::collections::HashMap<String, NoteId> = std::collections::HashMap::new();
        for seed in 0u32.. {
            let id = seeded_id(seed);
            let lead = wordid::encode(id.bytes())
                .split(crate::wordid::SEP)
                .next()
                .unwrap()
                .to_string();
            if let Some(&prev) = seen.get(&lead) {
                return (prev, id, lead);
            }
            seen.insert(lead, id);
        }
        unreachable!("2049 ids must collide in 2048 leading words")
    }

    #[test]
    fn resolve_card_accepts_bare_word_id_and_prefix() {
        let card = NoteId::new([0x11; 32]);
        let words = wordid::encode(card.bytes());
        let view = view_with_cards(&[card, NoteId::new([0x22; 32])]);

        // The full bare word-id (no board segment) resolves against the already
        // routed board — what `headway --board commerce show <word-id>` reaches
        // for without repeating the board.
        assert_eq!(resolve_card(&view, &words), Ok(card));

        // A hyphenated prefix resolves like a git short hash when unique.
        let two_words = words.rsplit_once(crate::wordid::SEP).unwrap().0;
        assert!(two_words.contains(crate::wordid::SEP));
        assert_eq!(resolve_card(&view, two_words), Ok(card));

        // Even a single leading word resolves when unique — a non-hex selector is
        // a word-id, not a hex prefix. (The two seeded ids differ in word one.)
        let first_word = words.split(crate::wordid::SEP).next().unwrap();
        assert_eq!(resolve_card(&view, first_word), Ok(card));

        // The scheme-less and full-scheme refs still route to the same card.
        assert_eq!(resolve_card(&view, &format!("commerce/{words}")), Ok(card));
        assert_eq!(
            resolve_card(&view, &format!("headway:commerce/{words}")),
            Ok(card)
        );

        // Case is normalised.
        assert_eq!(resolve_card(&view, &words.to_uppercase()), Ok(card));
    }

    #[test]
    fn resolve_card_word_id_errors_are_helpful() {
        let (a, b, lead) = colliding_leading_word();
        let view = view_with_cards(&[a, b, NoteId::new([0xee; 32])]);

        // A hyphenated prefix matching more than one card reports the candidates.
        let err = resolve_card(&view, &format!("{lead}{}", crate::wordid::SEP)).unwrap_err();
        assert!(err.starts_with("ambiguous word-id"), "{err}");
        assert!(err.contains(", "), "lists the matches: {err}");

        // A near-miss (real leading word, bogus tail) suggests cards sharing it.
        let err = resolve_card(&view, &format!("{lead}-nope-nope")).unwrap_err();
        assert!(err.starts_with("no card matching word-id"), "{err}");
        assert!(
            err.contains("did you mean:"),
            "suggests near matches: {err}"
        );

        // A word-id with no shared leading word falls back to the generic hint.
        let err = resolve_card(&view, "zzzzz-nope-nope").unwrap_err();
        assert!(err.contains("run `show`"), "{err}");
    }
}
