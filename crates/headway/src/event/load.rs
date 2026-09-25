//! ndb loading: the filters headway queries and subscribes with, the folds that
//! walk them into a [`BoardReducer`] (own and shared boards), incremental
//! [`reduce_delta`], and the point reads of a single stored note (board
//! preference, raw blocker and related sets).

use nostrdb::{Filter, Ndb, Note, NoteKey, Transaction};
use nostrdb_net::{NoteId, Pubkey};

use super::kinds::{
    HEADWAY_KINDS, KIND_BLOCKERS, KIND_BOARD, KIND_BOARD_PREF, KIND_COMMENT, KIND_COVER_NOTE,
    KIND_ISSUE, KIND_LABEL, KIND_PLACEMENT, KIND_RELATED, KIND_RELATION, KIND_SEQUENCE,
};
use super::model::BoardCoord;
use super::parse::{BlockerSet, RelatedSet, parse, parse_blockers, parse_related};
use super::reduce::BoardReducer;
use super::resolve::pick_board;
use super::view::BoardView;

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
/// this *raw* set rather than the folded [`CardView::blocked_by`](super::CardView::blocked_by), which drops
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
/// *raw* stored set rather than the folded [`CardView::related`](super::CardView::related) — the fold unions
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
///   ([`build_issue`](super::build_issue), [`build_placement`](super::build_placement)) rather than being owner-authored.
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
/// `E` = the issue id ([`build_comment`](super::build_comment)), whereas the lowercase parent `e` is the
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
/// the reducer runs in [`Authority::TeamKey`](super::reduce::Authority::TeamKey) mode, so any of them may amend any
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
/// makes [`Authority::TeamKey`](super::reduce::Authority::TeamKey) sound.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_config;
    use nostrdb_net::FullKeypair;

    use nostrdb::NoteBuilder;

    use crate::event::build::{
        build_blockers, build_board, build_comment, build_issue, build_placement, build_related,
        build_subject_edit,
    };
    use crate::event::model::{ColumnDef, board_address};

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
}
