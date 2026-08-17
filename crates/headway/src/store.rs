//! Persistence for headway boards.
//!
//! This is the app-layer bridge between the pure schema in [`crate::event`] and
//! nostrdb. It seeds the default board and translates UI intents
//! ([`BoardAction`]) into signed nostr events that are ingested into a local
//! nostrdb. Every ingested event is also handed to a [`Publisher`], the single
//! seam for fanning changes outward to a relay: the egui app ingests straight
//! into the nostrdb its embedded relay serves and so uses [`NoPublish`], while
//! the CLI keeps its own nostrdb and publishes each event to the running app's
//! relay over its websocket.

use std::collections::HashSet;

use enostr::{NoteId, Pubkey};
use nostrdb::{IngestMetadata, Ndb, Note, NoteBuilder};

use crate::event::{
    self, BoardView, COL_DELETED, CardView, ColumnDef, Container, Date, Field, Priority,
    board_address, build_archive_placement, build_blockers, build_board, build_comment,
    build_cover_note, build_field, build_issue, build_labels, build_placement, build_relation,
    build_sequence, build_subject_edit, rank_between,
};

/// The single board headway manages for now. Multi-board support will turn this
/// into a per-board identifier carried on [`crate::Headway`].
pub const BOARD_ID: &str = "headway";

/// A UI intent to mutate the board. Collected during rendering and applied
/// afterwards by [`apply`], which turns each variant into one or more ingested
/// events.
pub enum BoardAction {
    /// Move `card` into `to_col` so it lands at display row `to_row`.
    MoveCard {
        card: NoteId,
        to_col: usize,
        to_row: usize,
    },
    /// Create a new card titled `title` at the end of column `col`, optionally
    /// tagging it with `labels` and/or parenting it under `parent` (a subissue
    /// created in one step).
    AddCard {
        col: usize,
        title: String,
        labels: Vec<String>,
        parent: Option<NoteId>,
    },
    /// Replace a card's title (subject edit).
    EditTitle { card: NoteId, title: String },
    /// Replace a card's description (cover note).
    EditDescription { card: NoteId, description: String },
    /// Set a card's labels (additive union with any existing labels).
    SetLabels { card: NoteId, labels: Vec<String> },
    /// Set a card's priority (latest-authorised-wins; `Priority::None` clears it).
    SetPriority { card: NoteId, priority: Priority },
    /// Set a card's due date, or clear it with `None`.
    SetDue { card: NoteId, due: Option<Date> },
    /// Set a card's estimate, or clear it with `None`.
    SetEstimate { card: NoteId, estimate: Option<u32> },
    /// Position `card` within `container`'s work-order at `rank` (a fractional
    /// [`rank_between`] string, computed by [`seq_rank`]). Writes one sequence
    /// overlay; does not touch column placement.
    SetSequence {
        card: NoteId,
        container: Container,
        rank: String,
    },
    /// Sequence every subissue of `parent` into the exact work-order `order`
    /// (top-first). Writes one sequence overlay per child with fresh, evenly
    /// spaced ranks, promoting a partly- or un-sequenced list to a fully explicit
    /// order in one step. This is what a drag-reorder emits when the list isn't
    /// already fully sequenced: a lone [`Self::SetSequence`] can't land a card
    /// between two *unsequenced* siblings (they have no rank to insert between,
    /// and a sequenced child always sorts ahead of an unsequenced one), so the
    /// whole list is normalised instead.
    ReorderSubissues { parent: NoteId, order: Vec<NoteId> },
    /// Make `card` a subissue of `parent`, or detach it when `parent` is `None`.
    /// Refused (no events) when it would create a parent cycle.
    SetParent {
        card: NoteId,
        parent: Option<NoteId>,
    },
    /// Add `on` to `card`'s blocker set (a dependency edge: `card` is blocked by
    /// `on`). A no-op when the edge already exists or would create a cycle. The
    /// edge is independent of the parent axis and may point at another board.
    Block { card: NoteId, on: NoteId },
    /// Remove `on` from `card`'s blocker set. A no-op when the edge isn't present.
    Unblock { card: NoteId, on: NoteId },
    /// Post a NIP-22 comment on `card`. `reply_to`, when set, is another comment
    /// on the same card that this one threads under.
    AddComment {
        card: NoteId,
        body: String,
        reply_to: Option<NoteId>,
    },
    /// Remove a card from the board (tombstone placement).
    DeleteCard { card: NoteId },
    /// Archive a card: take it off the board but keep it recoverable, recording
    /// the column it came from so a restore can put it back.
    ArchiveCard { card: NoteId },
    /// Restore an archived card to the column it was archived from (or the first
    /// column if that column no longer exists).
    RestoreCard { card: NoteId },
    /// Append a new column named `name`.
    AddColumn { name: String },
    /// Rename the column at `col`.
    RenameColumn { col: usize, name: String },
    /// Remove the column at `col`. Its cards become unplaced and fall back to
    /// the first column on the next reduce.
    RemoveColumn { col: usize },
    /// Move the column at `from` to index `to`.
    MoveColumn { from: usize, to: usize },
    /// Rename the board itself: republish its definition with a new display
    /// `title`, preserving the slug, columns, and description.
    RenameBoard { title: String },
}

/// A sink for events that have been ingested locally and should also be fanned
/// out — typically published to a relay. [`ingest`] hands every event it stores
/// to the publisher as a ready-to-send NIP-01 `["EVENT", {...}]` frame, in the
/// order they were ingested.
pub trait Publisher {
    /// Called once per successfully ingested event with its `["EVENT", {...}]`
    /// JSON frame, ready to write to a relay websocket.
    fn publish(&mut self, event_frame: &str);
}

/// A [`Publisher`] that drops everything: local ingest only, no fan-out. Used by
/// the egui app, whose embedded relay already serves the same nostrdb it ingests
/// into, so there is nothing to publish.
pub struct NoPublish;

impl Publisher for NoPublish {
    fn publish(&mut self, _event_frame: &str) {}
}

/// The SNS channel a shared board's edits are sealed into: the team keys derived
/// from the board's `team_root`. Held by a [`Signer`] to switch [`ingest_signed`]
/// from publishing plaintext events to publishing kind-1081 SNS envelopes.
///
/// Only the keys live here — the authoring member is the [`Signer`]'s own secret,
/// so the seal inside each envelope attributes the edit to the real author while
/// the envelope is signed by (and addressed to) the shared team keypair.
pub struct SnsChannel {
    /// Team keypair + envelope key, from [`enostr::sns::derive_sns_keys`].
    pub keys: enostr::sns::SnsKeys,
}

/// Who is writing an edit, and how it reaches the wire: the signing `secret`
/// (which also identifies the author) plus, for a shared board, the [`SnsChannel`]
/// to seal edits into. A `None` channel publishes plaintext — the single-writer
/// path every board used before SNS.
pub struct Signer<'a> {
    secret: &'a [u8; 32],
    channel: Option<&'a SnsChannel>,
}

impl<'a> Signer<'a> {
    /// A plaintext single-writer signer: events are ingested and published as-is.
    pub fn plain(secret: &'a [u8; 32]) -> Self {
        Self {
            secret,
            channel: None,
        }
    }

    /// A shared-board signer: each event is sealed into an SNS envelope for
    /// `channel` before it is ingested and published.
    pub fn shared(secret: &'a [u8; 32], channel: &'a SnsChannel) -> Self {
        Self {
            secret,
            channel: Some(channel),
        }
    }

    /// A signer that seals into `channel` when present and publishes plaintext
    /// otherwise — the form the app uses, where a board is shared or not.
    pub fn new(secret: &'a [u8; 32], channel: Option<&'a SnsChannel>) -> Self {
        Self { secret, channel }
    }
}

/// Sign `builder` with `secret` and ingest+publish the resulting note as
/// plaintext — the single-writer path. Shared boards go through
/// [`ingest_signed`] with a [`Signer::shared`] instead.
pub fn ingest(
    ndb: &Ndb,
    builder: NoteBuilder,
    secret: &[u8; 32],
    publisher: &mut dyn Publisher,
) -> Option<NoteId> {
    ingest_signed(ndb, builder, &Signer::plain(secret), publisher)
}

/// Sign `builder`, ingest the resulting note into the local nostrdb, and hand its
/// `["EVENT", {...}]` frame to `publisher`. Returns the note id, or `None` if
/// building/ingesting failed (in which case nothing is published).
///
/// For a plaintext [`Signer`] the note itself is ingested and published. For a
/// [`Signer::shared`] the signed note is the *rumor*: it is sealed into a
/// kind-1081 SNS envelope ([`enostr::sns::wrap_rumor`]), and that envelope is what
/// gets ingested (nostrdb auto-unwraps it back to the rumor, since the team_root
/// is registered) and published. Either way the returned id is the rumor's, which
/// nostrdb recomputes identically on unwrap — so a card's id is stable whether the
/// board is shared or not. The rumor must be a complete signed note (nostrdb
/// re-parses it on the seal peel and requires every field but the sig/pubkey,
/// including the id), which `builder.sign(...).build()` guarantees.
pub fn ingest_signed(
    ndb: &Ndb,
    builder: NoteBuilder,
    signer: &Signer,
    publisher: &mut dyn Publisher,
) -> Option<NoteId> {
    let note = builder.sign(signer.secret).build()?;
    let id = NoteId::new(*note.id());
    let frame = match signer.channel {
        None => enostr::ClientMessage::event(&note).ok()?.to_json().ok()?,
        Some(channel) => {
            let member = enostr::FullKeypair::from_secret_bytes(signer.secret)?;
            let rumor_json = note.json().ok()?;
            let envelope =
                enostr::sns::wrap_rumor(&channel.keys, &member, &rumor_json, note.created_at())?;
            enostr::ClientMessage::event(&envelope)
                .ok()?
                .to_json()
                .ok()?
        }
    };
    ndb.process_event_with(&frame, IngestMetadata::new().client(true))
        .ok()?;
    publisher.publish(&frame);
    Some(id)
}

/// Persist `coord` as `author`'s selected-board preference: a replaceable
/// kind-30623 note ([`event::build_board_pref`]) whose content is the board's
/// coordinate, signed by the account key and PNS-wrapped, ingested into the local
/// nostrdb. This is the on-nostrdb replacement for the old `headway-boards.json`;
/// [`event::load_board_pref`] reads it back latest-wins. Carrying the full
/// coordinate (not just the slug) lets the restored selection disambiguate an own
/// board from a joined shared board of the same slug.
///
/// Callers pass [`NoPublish`], so the note stays local — it is never fanned out
/// to a relay, and its inner kind isn't in `headway_filter`, so the board sync
/// never picks it up either. A watch-only account has no `secret` and so can't
/// save one (it never had a real preference to persist); callers guard on the
/// signer.
pub fn save_board_pref(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    coord: &event::BoardCoord,
    publisher: &mut dyn Publisher,
) {
    // Stamp strictly past the current preference so a same-second re-save (rapid
    // board switching) still supersedes rather than tying latest-wins.
    let created_at = next_after(event::board_pref_created_at(ndb, author));
    let coordinate = coord.coordinate();
    let Some(inner) = event::build_board_pref(&coordinate)
        .created_at(created_at)
        .sign(secret)
        .build()
    else {
        return;
    };
    ingest_pns(ndb, &inner, secret, publisher);
}

/// PNS-wrap a signed `inner` note and ingest the kind-1080 wrapper into the local
/// nostrdb, then publish it. The crypto + wrapper construction lives in
/// [`enostr::pns::wrap`]; this only adds the app-specific ingest/publish glue (the
/// [`Publisher`] seam differs per crate, so it can't be shared). nostrdb
/// transparently unwraps the envelope on read once the account key is registered
/// via `Ndb::add_key`, so the inner note stays queryable. Returns the inner note's
/// id.
fn ingest_pns(
    ndb: &Ndb,
    inner: &Note,
    device_secret: &[u8; 32],
    publisher: &mut dyn Publisher,
) -> Option<NoteId> {
    let inner_id = NoteId::new(*inner.id());
    let pns_keys = enostr::pns::derive_pns_keys(device_secret);
    let wrapper = enostr::pns::wrap(&pns_keys, &inner.json().ok()?, now_secs())?;
    let frame = enostr::ClientMessage::event(&wrapper)
        .ok()?
        .to_json()
        .ok()?;
    ndb.process_event_with(&frame, IngestMetadata::new().client(true))
        .ok()?;
    publisher.publish(&frame);
    Some(inner_id)
}

/// The default columns a fresh board is seeded with.
fn default_columns() -> Vec<ColumnDef> {
    vec![
        ColumnDef::new("backlog", "Backlog"),
        ColumnDef::new("todo", "Todo"),
        ColumnDef::new("in-progress", "In Progress"),
        ColumnDef::new("in-review", "In Review"),
        ColumnDef::new("done", "Done"),
    ]
}

/// Seed a fresh default board for `author` into the local nostrdb: just the
/// board event with its columns, no cards. Cards are added later via
/// [`BoardAction::AddCard`]. Titled "Headway"; use [`seed_board`] to name it.
pub fn seed_default_board(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    board_id: &str,
    publisher: &mut dyn Publisher,
) -> usize {
    seed_board(ndb, author, secret, board_id, "Headway", publisher)
}

/// Derive a board slug (its stable `d`-tag id) from a human `title`, avoiding any
/// slug the `taken` predicate rejects. Lowercase ASCII alphanumerics are kept;
/// every other run collapses to a single `-`. Empty/all-punctuation input falls
/// back to `board`, and a clash gets a `-2`, `-3`, … suffix. Shared so any
/// surface that turns a typed name into a board (e.g. the GUI's "New board") gets
/// the same rule; the CLI takes a slug verbatim and doesn't need it.
pub fn board_slug(title: &str, taken: impl Fn(&str) -> bool) -> String {
    let mut base = String::new();
    let mut prev_dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            base.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !base.is_empty() && !prev_dash {
            base.push('-');
            prev_dash = true;
        }
    }
    let base = base.trim_end_matches('-');
    let base = if base.is_empty() { "board" } else { base };

    if !taken(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|slug| !taken(slug))
        .expect("an unused slug exists")
}

/// Seed a fresh board with a display `title` (the `board_id` is its stable slug).
/// Like [`seed_default_board`] but lets a caller create a *named* board — e.g. a
/// separate "Work" board alongside the default one, all under one identity.
///
/// Returns the number of events ingested (0 or 1). Callers awaiting an async
/// writer (tests) use the count to know when the whole seed has committed; the
/// GUI/CLI ignore it.
pub fn seed_board(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    board_id: &str,
    title: &str,
    publisher: &mut dyn Publisher,
) -> usize {
    let _ = author;
    let columns = default_columns();
    ingest(
        ndb,
        build_board(board_id, title, "", &columns),
        secret,
        publisher,
    )
    .is_some() as usize
}

/// Seed a default board *and* a fixed set of demo cards. The product seed
/// ([`seed_default_board`]) is deliberately card-less; this is the populated
/// board used by tests and demos. Cards land 3 / 2 / 1 / 0 / 1 across the
/// columns, in seeded order (increasing ranks per column).
///
/// Every card event is stamped `at` rather than the wall clock, so a seeded
/// board is identical from one run to the next: event ids (and the word-ids
/// derived from them) and the created/updated times the UI renders only stay
/// stable across runs if the seed's timestamps do. Snapshot tests pin `at`
/// (together with [`crate::fmt::freeze_now`]) for reproducible frames.
///
/// Returns the number of events ingested. Ingest is async (a writer thread), so
/// a test can't tell the seed has fully committed by inspecting board state —
/// trailing events land after any given card appears. The count is the race-free
/// signal instead: wait until the writer has delivered exactly this many notes
/// and the whole seed is in, no matter the order or which event is last.
pub fn seed_demo_board(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    board_id: &str,
    at: u64,
    publisher: &mut dyn Publisher,
) -> usize {
    let mut ingested = seed_default_board(ndb, author, secret, board_id, publisher);

    // Tally every event the seed writes. `ingest!` wraps [`ingest`], counting each
    // success; using it for every ingest below keeps the returned count correct as
    // seed events are added or removed — nothing has to be hand-maintained.
    macro_rules! ingest {
        ($builder:expr) => {{
            let id = ingest(ndb, $builder, secret, publisher);
            ingested += id.is_some() as usize;
            id
        }};
    }

    let addr = board_address(author, board_id);
    // The cards are created a fortnight before `at` and amended at instants in
    // between (below), so the demo board's activity timelines — and the
    // relative times next to them — have real depth instead of a wall of
    // "just now". The drag card starts in Todo and the event-model card with
    // an earlier title/description/label set; the post-creation amendments
    // resolve every card to the exact state listed here-adjacent, so tests
    // addressing cards by their final title/column are unaffected.
    let created = at - 14 * 86_400;
    let cards: [(&str, &str, &str, &[&str]); 7] = [
        (
            "backlog",
            "Nostr event model",
            "Decide how boards, columns and cards map to nostr events.",
            &["protocol"],
        ),
        ("backlog", "Sync cards across relays", "", &["nostr"]),
        ("backlog", "Card detail / comments view", "", &["ui"]),
        ("todo", "Inline card creation", "", &["ui"]),
        ("todo", "Column reordering", "", &[]),
        (
            "todo",
            "Drag-and-drop between columns",
            "Reorder within a lane and move across lanes with a live insertion line.",
            &["ux"],
        ),
        ("done", "Scaffold the Headway app crate", "", &["chore"]),
    ];

    // Hand out increasing ranks per column so cards keep their seeded order.
    // Stamp each card's creation events with the one shared timestamp: a label
    // event that landed a second after its issue would count as an "update"
    // (and an activity row) rather than part of creation.
    let mut last_rank: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    let mut ids: std::collections::HashMap<&str, NoteId> = std::collections::HashMap::new();
    for (col_id, title, body, labels) in cards {
        let Some(id) = ingest!(build_issue(&addr, title, body).created_at(created)) else {
            continue;
        };
        ids.insert(title, id);
        let rank = rank_between(last_rank.get(col_id).map(|s| s.as_str()), None);
        ingest!(build_placement(board_id, &addr, &id, col_id, &rank).created_at(created));
        if !labels.is_empty() {
            ingest!(build_labels(&id, labels).created_at(created));
        }
        last_rank.insert(col_id, rank);
    }

    // Parent the sync and scaffold cards under the event-model card so the demo
    // board exercises subissue rendering: a 1/2 rollup on the parent (the
    // scaffold child sits in Done, the sync child in Backlog) and a parent
    // breadcrumb on each child.
    if let Some(parent) = ids.get("Nostr event model") {
        for child in ["Sync cards across relays", "Scaffold the Headway app crate"] {
            if let Some(child) = ids.get(child) {
                ingest!(build_relation(child, Some(parent)).created_at(created));
            }
        }
    }

    // Post-creation history: amend the event-model card (rename → label swap →
    // description edit) and move the drag card into In Progress, each at its
    // own instant, so the detail views showcase a populated activity timeline.
    // These land the cards on their final, test-visible state.
    if let Some(card) = ids.get("Nostr event model") {
        ingest!(
            build_subject_edit(card, "Define nostr event model for boards")
                .created_at(at - 10 * 86_400)
        );
        ingest!(build_labels(card, &["nostr"]).created_at(at - 6 * 86_400));
        ingest!(
            build_cover_note(
                card,
                author,
                "Decide how boards, columns and cards map to nostr events. \
             Likely an addressable (NIP-33) board event plus per-card events.",
            )
            .created_at(at - 3 * 86_400)
        );
    }
    if let Some(card) = ids.get("Drag-and-drop between columns") {
        let rank = rank_between(None, None);
        ingest!(
            build_placement(board_id, &addr, card, "in-progress", &rank).created_at(at - 86_400)
        );
    }

    // Prioritise a few cards so the board and detail views showcase the priority
    // glyph across levels (the rest stay at the unadorned `Priority::None`).
    for (title, priority) in [
        // Keyed by each card's *seed* title (`ids`), before the rename above.
        ("Nostr event model", Priority::Urgent),
        ("Drag-and-drop between columns", Priority::High),
        ("Card detail / comments view", Priority::Medium),
        ("Column reordering", Priority::Low),
    ] {
        if let Some(card) = ids.get(title) {
            ingest!(
                build_field(card, Field::Priority, priority.as_str()).created_at(at - 2 * 86_400)
            );
        }
    }

    ingested
}

/// Apply one [`BoardAction`] against the current `view`, ingesting the events it
/// implies. `view` is the pre-action snapshot, used to compute insertion ranks
/// and to reconstruct the column list for board-level edits.
pub fn apply(
    ndb: &Ndb,
    board_id: &str,
    view: &BoardView,
    author: &Pubkey,
    signer: &Signer,
    action: BoardAction,
    publisher: &mut dyn Publisher,
) {
    // The board `#a` coordinate is the *owner's*, not the signer's. On a shared
    // board the signer is a member, but the board (and every edit anchored to it)
    // lives under `30619:<owner>:<slug>`, which the folded `view` carries as
    // `view.author`. Anchoring at the member's own key would tag the edit at a
    // phantom coordinate `fold_shared_board(owner)` never gathers. On an own board
    // `view.author == author`, so this is a no-op there. `author`/`signer` stay the
    // acting editor, preserving authorship and seal attribution.
    let addr = board_address(&Pubkey::new(view.author), board_id);

    match action {
        BoardAction::MoveCard {
            card,
            to_col,
            to_row,
        } => {
            let Some(col) = view.columns.get(to_col) else {
                return;
            };
            let rank = rank_for_insert(
                &col.cards,
                |c| c.id,
                |c| c.rank.as_str(),
                Some(card),
                to_row,
            );
            let after = find_card(view, card).map_or(0, |c| c.placed_at);
            ingest_signed(
                ndb,
                build_placement(board_id, &addr, &card, &col.id, &rank)
                    .created_at(next_after(after)),
                signer,
                publisher,
            );
        }
        BoardAction::AddCard {
            col,
            title,
            labels,
            parent,
        } => {
            let Some(c) = view.columns.get(col) else {
                return;
            };
            // A brand-new card can't be anyone's ancestor, so parenting it needs
            // no cycle check — just that the parent actually exists.
            let parent = parent.filter(|p| find_card_any(view, *p).is_some());
            let Some(id) = ingest_signed(ndb, build_issue(&addr, &title, ""), signer, publisher)
            else {
                return;
            };
            let rank =
                rank_for_insert(&c.cards, |c| c.id, |c| c.rank.as_str(), None, c.cards.len());
            ingest_signed(
                ndb,
                build_placement(board_id, &addr, &id, &c.id, &rank),
                signer,
                publisher,
            );
            if !labels.is_empty() {
                ingest_signed(ndb, build_labels(&id, &labels), signer, publisher);
            }
            if let Some(parent) = parent {
                ingest_signed(ndb, build_relation(&id, Some(&parent)), signer, publisher);
            }
        }
        BoardAction::EditTitle { card, title } => {
            ingest_signed(ndb, build_subject_edit(&card, &title), signer, publisher);
        }
        BoardAction::EditDescription { card, description } => {
            ingest_signed(
                ndb,
                build_cover_note(&card, author, &description),
                signer,
                publisher,
            );
        }
        BoardAction::SetLabels { card, labels } => {
            ingest_signed(ndb, build_labels(&card, &labels), signer, publisher);
        }
        BoardAction::SetPriority { card, priority } => {
            let f = build_field(&card, Field::Priority, priority.as_str());
            ingest_signed(ndb, f, signer, publisher);
        }
        BoardAction::SetDue { card, due } => {
            let value = due.map(|d| d.to_string()).unwrap_or_default();
            ingest_signed(
                ndb,
                build_field(&card, Field::Due, &value),
                signer,
                publisher,
            );
        }
        BoardAction::SetEstimate { card, estimate } => {
            let value = estimate.map(|e| e.to_string()).unwrap_or_default();
            let f = build_field(&card, Field::Estimate, &value);
            ingest_signed(ndb, f, signer, publisher);
        }
        BoardAction::SetSequence {
            card,
            container,
            rank,
        } => {
            // Board-agnostic overlay: keyed by (container, card), no board `a`
            // tag, so it needs no board context here. The caller precomputes
            // `rank` via `seq_rank` against the container's current members.
            ingest_signed(
                ndb,
                build_sequence(&container, &card, &rank),
                signer,
                publisher,
            );
        }
        BoardAction::ReorderSubissues { parent, order } => {
            // Re-rank the whole container in one shot: walk `order` top-first,
            // appending each child after the previous with the shared
            // `rank_between` kernel. Fresh, strictly increasing ranks give the
            // container a fully explicit order, so a later single-card drag can
            // insert against sequenced neighbours (see `SetSequence`).
            let container = Container::Card(*parent.bytes());
            let mut prev: Option<String> = None;
            for card in &order {
                let rank = rank_between(prev.as_deref(), None);
                ingest_signed(
                    ndb,
                    build_sequence(&container, card, &rank),
                    signer,
                    publisher,
                );
                prev = Some(rank);
            }
        }
        BoardAction::SetParent { card, parent } => {
            if let Some(parent) = parent {
                if would_cycle(view, card, parent) {
                    return;
                }
                ingest_signed(ndb, build_relation(&card, Some(&parent)), signer, publisher);
            } else {
                ingest_signed(ndb, build_relation(&card, None), signer, publisher);
            }
        }
        BoardAction::Block { card, on } => {
            // Refuse an edge that would close a dependency loop (same-board only;
            // see `would_block_cycle`).
            if would_block_cycle(view, card, on) {
                return;
            }
            // Rebuild from the *raw* stored set, not the folded `blocked_by`: the
            // fold drops edges it couldn't resolve (e.g. a cross-board blocker),
            // so editing the resolved view would silently discard them.
            let cur = event::current_blockers(ndb, author, &card);
            let mut set: Vec<NoteId> = cur
                .as_ref()
                .map(|b| b.blockers.iter().map(|id| NoteId::new(*id)).collect())
                .unwrap_or_default();
            if set.contains(&on) {
                return;
            }
            set.push(on);
            republish_blockers(ndb, &card, &set, cur, signer, publisher);
        }
        BoardAction::Unblock { card, on } => {
            let cur = event::current_blockers(ndb, author, &card);
            let mut set: Vec<NoteId> = cur
                .as_ref()
                .map(|b| b.blockers.iter().map(|id| NoteId::new(*id)).collect())
                .unwrap_or_default();
            let before = set.len();
            set.retain(|id| *id != on);
            if set.len() == before {
                return;
            }
            republish_blockers(ndb, &card, &set, cur, signer, publisher);
        }
        BoardAction::AddComment {
            card,
            body,
            reply_to,
        } => {
            // The comment is rooted on the issue, so we need the issue author
            // (the card's author) for the NIP-22 root `P`. Unknown card -> no-op.
            let Some(c) = find_card_any(view, card) else {
                return;
            };
            let issue_author = Pubkey::new(c.author);

            // A reply additionally names the parent comment's author. If the
            // parent isn't on the card we know about, drop the reply rather than
            // mis-attribute it.
            let parent_author;
            let reply = match &reply_to {
                Some(parent) => {
                    let Some(pc) = c.comments.iter().find(|c| c.id == *parent) else {
                        return;
                    };
                    parent_author = Pubkey::new(pc.author);
                    Some((parent, &parent_author))
                }
                None => None,
            };

            // Comments fold in `created_at` order (id as tiebreaker). Nostr
            // timestamps are whole seconds, so two comments posted in the same
            // second would tie and the id tiebreaker would order them at random —
            // a reply could sort ahead of the comment it answers. Stamp strictly
            // past the newest comment already on the card so order stays causal
            // (mirrors [`next_after`]).
            let latest = c.comments.iter().map(|c| c.created_at).max().unwrap_or(0);
            ingest_signed(
                ndb,
                build_comment(&card, &issue_author, reply, &body).created_at(next_after(latest)),
                signer,
                publisher,
            );
        }
        BoardAction::DeleteCard { card } => {
            // build_placement needs a rank; reuse the card's current one (or a
            // midpoint) — the column is the tombstone sentinel either way.
            let c = find_card(view, card);
            let rank = non_empty_rank(c.map_or("", |c| c.rank.as_str()));
            let after = c.map_or(0, |c| c.placed_at);
            ingest_signed(
                ndb,
                build_placement(board_id, &addr, &card, COL_DELETED, &rank)
                    .created_at(next_after(after)),
                signer,
                publisher,
            );
        }
        BoardAction::ArchiveCard { card } => {
            // Capture the card's current column so a restore can return it there.
            let Some((from_col, c)) = find_card_col(view, card) else {
                return;
            };
            let rank = non_empty_rank(&c.rank);
            ingest_signed(
                ndb,
                build_archive_placement(board_id, &addr, &card, from_col, &rank)
                    .created_at(next_after(c.placed_at)),
                signer,
                publisher,
            );
        }
        BoardAction::RestoreCard { card } => {
            let Some(entry) = view.archived.iter().find(|a| a.card.id == card) else {
                return;
            };
            // Restore to the origin column, falling back to the first column if
            // that column is gone (the reducer would reflow it there anyway).
            let to_col = entry
                .from
                .as_deref()
                .filter(|id| view.columns.iter().any(|c| c.id == *id))
                .or_else(|| view.columns.first().map(|c| c.id.as_str()));
            let Some(to_col) = to_col else {
                return;
            };
            let rank = non_empty_rank(&entry.card.rank);
            ingest_signed(
                ndb,
                build_placement(board_id, &addr, &card, to_col, &rank)
                    .created_at(next_after(entry.card.placed_at)),
                signer,
                publisher,
            );
        }
        BoardAction::AddColumn { name } => {
            let mut cols = column_defs(view);
            cols.push(ColumnDef::new(unique_col_id(&cols, &name), name));
            republish_board(ndb, board_id, view, signer, &cols, publisher);
        }
        BoardAction::RenameColumn { col, name } => {
            let mut cols = column_defs(view);
            let Some(def) = cols.get_mut(col) else {
                return;
            };
            def.name = name;
            republish_board(ndb, board_id, view, signer, &cols, publisher);
        }
        BoardAction::RemoveColumn { col } => {
            let mut cols = column_defs(view);
            if col >= cols.len() {
                return;
            }
            cols.remove(col);
            republish_board(ndb, board_id, view, signer, &cols, publisher);
        }
        BoardAction::MoveColumn { from, to } => {
            let mut cols = column_defs(view);
            if from >= cols.len() || to >= cols.len() || from == to {
                return;
            }
            let def = cols.remove(from);
            cols.insert(to, def);
            republish_board(ndb, board_id, view, signer, &cols, publisher);
        }
        BoardAction::RenameBoard { title } => {
            // Same addressable-event republish as `republish_board`, but swapping
            // the title instead of the columns. Bump `created_at` past the current
            // board so the reducer keeps the renamed version (see `republish_board`).
            let cols = column_defs(view);
            let created_at = now_secs().max(view.created_at + 1);
            ingest_signed(
                ndb,
                build_board(board_id, &title, &view.description, &cols).created_at(created_at),
                signer,
                publisher,
            );
        }
    }
}

/// A board to operate on across a cross-board action: its `id` (slug) paired with
/// its current folded `view`. Bundled so [`link_card`]/[`move_card_between_boards`]
/// take one argument per board rather than an id/view pair each.
#[derive(Clone, Copy)]
pub struct BoardRef<'a> {
    pub id: &'a str,
    pub view: &'a BoardView,
}

/// Place `card` onto `target`, in the column whose id matches `prefer_col` when
/// the board has one, else the target's first column, at the end. Returns the
/// placement's note id.
fn place_card(
    ndb: &Ndb,
    target: BoardRef,
    prefer_col: Option<&str>,
    signer: &Signer,
    card: NoteId,
    publisher: &mut dyn Publisher,
) -> Option<NoteId> {
    // Anchor the placement at the target board's *owner* coordinate (see `apply`),
    // which the folded `target.view` carries — not the signing member's own key.
    let addr = board_address(&Pubkey::new(target.view.author), target.id);
    let col = prefer_col
        .and_then(|id| target.view.columns.iter().find(|c| c.id == id))
        .or_else(|| target.view.columns.first())?;
    let rank = rank_for_insert(
        &col.cards,
        |c| c.id,
        |c| c.rank.as_str(),
        Some(card),
        col.cards.len(),
    );
    let after = find_card(target.view, card).map_or(0, |c| c.placed_at);
    ingest_signed(
        ndb,
        build_placement(target.id, &addr, &card, &col.id, &rank).created_at(next_after(after)),
        signer,
        publisher,
    )
}

/// Link `card` from `source` onto `target`, preserving its column. This is a
/// *link*: membership is placement-driven, so the card keeps any other boards it
/// is already on, and the same issue (with all its overlays — title, labels,
/// comments) now shows on `target` too. It lands in the target column whose id
/// matches the card's column on `source`, falling back to the target's first
/// column when that column doesn't exist there. Returns the placement's note id.
///
/// Re-linking simply re-ranks (latest wins).
pub fn link_card(
    ndb: &Ndb,
    source: BoardRef,
    target: BoardRef,
    signer: &Signer,
    card: NoteId,
    publisher: &mut dyn Publisher,
) -> Option<NoteId> {
    let from_col = find_card_col(source.view, card).map(|(col, _)| col);
    place_card(ndb, target, from_col, signer, card, publisher)
}

/// Move `card` from the `source` board to the `target` board: link it onto
/// `target` (preserving its column, see [`link_card`]), then tombstone its
/// placement on `source`. The issue id and all its overlays are preserved — it's
/// the same card, just re-homed. Returns the new placement's note id.
pub fn move_card_between_boards(
    ndb: &Ndb,
    source: BoardRef,
    target: BoardRef,
    signer: &Signer,
    card: NoteId,
    publisher: &mut dyn Publisher,
) -> Option<NoteId> {
    let placed = link_card(ndb, source, target, signer, card, publisher)?;
    // Placement-driven membership: a tombstone on the source removes it from
    // `source` only, leaving the freshly-linked placement on `target`. The
    // tombstone must land on the source *owner's* coordinate (see `apply`), which
    // `source.view.author` carries — not the signing member's own key.
    let src_addr = board_address(&Pubkey::new(source.view.author), source.id);
    let c = find_card(source.view, card);
    let rank = non_empty_rank(c.map_or("", |c| c.rank.as_str()));
    let after = c.map_or(0, |c| c.placed_at);
    ingest_signed(
        ndb,
        build_placement(source.id, &src_addr, &card, COL_DELETED, &rank)
            .created_at(next_after(after)),
        signer,
        publisher,
    );
    Some(placed)
}

/// Republish the board event with a new column list, preserving title/description.
///
/// The board is an addressable event, so a republish supersedes the prior one by
/// `created_at`. Nostr timestamps are whole seconds, so a quick succession of
/// edits (or our own seed-then-edit in tests) would tie and the reducer would
/// keep the *old* board — dropping the edit. Stamp a timestamp strictly greater
/// than the version we're editing so the new board always wins.
fn republish_board(
    ndb: &Ndb,
    board_id: &str,
    view: &BoardView,
    signer: &Signer,
    columns: &[ColumnDef],
    publisher: &mut dyn Publisher,
) {
    let created_at = now_secs().max(view.created_at + 1);
    ingest_signed(
        ndb,
        build_board(board_id, &view.title, &view.description, columns).created_at(created_at),
        signer,
        publisher,
    );
}

/// The `created_at` to stamp on a re-placement that must supersede a prior
/// placement made at `prev`. Nostr timestamps are whole seconds, so a card
/// moved/deleted/archived in the same second it was last placed would *tie* the
/// reducer's latest-wins and silently no-op; stamp strictly past `prev` so the
/// new placement always wins (mirrors [`republish_board`]).
fn next_after(prev: u64) -> u64 {
    now_secs().max(prev + 1)
}

/// Current wall-clock time in whole seconds since the Unix epoch (nostr's
/// `created_at` unit). Falls back to 0 if the clock is before the epoch.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The current column definitions carried by `view`, ready to be edited and
/// republished.
fn column_defs(view: &BoardView) -> Vec<ColumnDef> {
    view.columns
        .iter()
        .map(|c| ColumnDef::new(c.id.clone(), c.name.clone()))
        .collect()
}

/// Find a card anywhere on the board by id.
fn find_card(view: &BoardView, card: NoteId) -> Option<&CardView> {
    view.columns
        .iter()
        .flat_map(|c| c.cards.iter())
        .find(|c| c.id == card)
}

/// Find a card by id across the live columns *and* the archived set — commenting
/// on an archived card is still valid, so it needs the wider search.
fn find_card_any(view: &BoardView, card: NoteId) -> Option<&CardView> {
    find_card(view, card).or_else(|| view.archived.iter().map(|a| &a.card).find(|c| c.id == card))
}

/// Publish `card`'s complete blocker set, superseding its previous one. Blocker
/// sets resolve latest-authorised-wins and nostr timestamps are whole seconds, so
/// the new event is stamped strictly past the one it replaces (`prev`) — a
/// same-second re-block/unblock still wins (mirrors [`next_after`]'s use for
/// comments/placements).
fn republish_blockers(
    ndb: &Ndb,
    card: &NoteId,
    set: &[NoteId],
    prev: Option<event::BlockerSet>,
    signer: &Signer,
    publisher: &mut dyn Publisher,
) {
    let after = prev.map_or(0, |b| b.created_at);
    ingest_signed(
        ndb,
        build_blockers(card, set).created_at(next_after(after)),
        signer,
        publisher,
    );
}

/// Would blocking `card` on `on` create a dependency cycle? A cycle would form
/// iff `on` is already (transitively) blocked by `card`, so this walks the
/// blocked-by graph outward from `on` looking for `card`. Refuses a self-block.
/// Like [`would_cycle`] it sees only this board's folded view; a blocker on
/// another board is treated as a leaf (its edges aren't followed), so cross-board
/// cycles aren't detected — the reducer renders any slipped-through cycle
/// harmlessly as mutual edges. Public so a GUI blocker picker can pre-filter its
/// candidates with the same rule the write path enforces.
pub fn would_block_cycle(view: &BoardView, card: NoteId, on: NoteId) -> bool {
    if card == on {
        return true;
    }
    let mut visited: HashSet<NoteId> = HashSet::new();
    let mut stack = vec![on];
    while let Some(id) = stack.pop() {
        if id == card {
            return true;
        }
        if !visited.insert(id) {
            continue;
        }
        // A blocker we can't resolve on this board (e.g. cross-board) is a leaf:
        // we can't follow its edges, so it can't extend a cycle we'd catch here.
        if let Some(c) = find_card_any(view, id) {
            stack.extend(c.blocked_by.iter().map(|e| e.id));
        }
    }
    false
}

/// Would parenting `card` under `parent` create a cycle? Walks the ancestor
/// chain upward from `parent` looking for `card`. The walk sees this board's
/// view only, so an ancestor placed solely on another board isn't followed —
/// good enough for the write-path guard (the reducer renders a slipped-through
/// cycle harmlessly, one level at a time). Also refuses an unknown parent, and
/// caps the walk so a pre-existing cycle can't spin it forever. Public so the
/// GUI's parent picker can filter its candidates with the same rule the write
/// path enforces.
pub fn would_cycle(view: &BoardView, card: NoteId, parent: NoteId) -> bool {
    let mut cur = Some(parent);
    for _ in 0..64 {
        let Some(id) = cur else {
            return false;
        };
        if id == card {
            return true;
        }
        let Some(c) = find_card_any(view, id) else {
            // Unknown ancestor: can't prove it's safe, refuse.
            return true;
        };
        cur = c.parent;
    }
    true
}

/// Find a card and the id of the column it currently sits in.
fn find_card_col(view: &BoardView, card: NoteId) -> Option<(&str, &CardView)> {
    view.columns.iter().find_map(|col| {
        col.cards
            .iter()
            .find(|c| c.id == card)
            .map(|c| (col.id.as_str(), c))
    })
}

/// A placement needs a rank; fall back to a midpoint when the card has none
/// (e.g. it was sitting unplaced in the fallback column).
fn non_empty_rank(rank: &str) -> String {
    if rank.is_empty() {
        "m".to_string()
    } else {
        rank.to_string()
    }
}

/// Compute a fractional rank that lands an item at display index `to_row` among
/// `items` (sorted by rank). `id_of`/`rank_of` read each item's stable id and its
/// current rank string, so this one function serves every ordering axis — a
/// column's cards (keyed on [`CardView::rank`]) today, and any container-scoped
/// sequence tomorrow — rather than a copy per axis. `moving` excludes the item
/// being moved from the neighbour search so an in-place move doesn't fence itself.
fn rank_for_insert<T>(
    items: &[T],
    id_of: impl Fn(&T) -> NoteId,
    rank_of: impl Fn(&T) -> &str,
    moving: Option<NoteId>,
    to_row: usize,
) -> String {
    let others: Vec<&T> = items.iter().filter(|&c| Some(id_of(c)) != moving).collect();

    // `to_row` indexes the displayed list (which still includes the moved item);
    // translate it into an index among `others`.
    let pos = match moving.and_then(|m| items.iter().position(|c| id_of(c) == m)) {
        Some(cur) if cur < to_row => to_row - 1,
        _ => to_row,
    };
    let pos = pos.min(others.len());

    let left = pos
        .checked_sub(1)
        .and_then(|i| others.get(i).copied())
        .map(&rank_of);
    let right = others.get(pos).copied().map(&rank_of);
    rank_between(left, right)
}

/// Where to insert a card within a container's sequenced work-order, resolved by
/// [`seq_rank`]. `After`/`Before` name an anchor that must itself already be
/// sequenced.
pub enum SeqPosition {
    /// Before every currently-sequenced member.
    First,
    /// After every currently-sequenced member.
    Last,
    /// Immediately after the given (already-sequenced) card.
    After(NoteId),
    /// Immediately before the given (already-sequenced) card.
    Before(NoteId),
}

/// A container member with its resolved seq rank, for [`seq_rank`]'s insert math.
struct SeqMember {
    id: NoteId,
    rank: String,
}

/// The container's currently-sequenced members, in work-order (ascending seq
/// rank). A board root's members are its top-level (non-subissue) cards; a card
/// container's are that card's subissues. Unsequenced members are excluded —
/// they have no rank to anchor an insert against.
fn sequenced_members(view: &BoardView, container: &Container) -> Vec<SeqMember> {
    match container {
        Container::Card(parent) => find_card(view, NoteId::new(*parent))
            .map(|c| {
                c.subissues
                    .iter()
                    .filter_map(|s| s.seq.clone().map(|rank| SeqMember { id: s.id, rank }))
                    .collect()
            })
            .unwrap_or_default(),
        Container::BoardRoot(_) => {
            let mut members: Vec<SeqMember> = view
                .columns
                .iter()
                .flat_map(|col| col.cards.iter())
                .filter(|c| c.parent.is_none())
                .filter_map(|c| c.seq.clone().map(|rank| SeqMember { id: c.id, rank }))
                .collect();
            members.sort_by(|a, b| a.rank.cmp(&b.rank));
            members
        }
    }
}

/// Compute the fractional rank that lands `card` at `position` within
/// `container`, using the container's currently-sequenced members and the shared
/// [`rank_for_insert`] kernel (the same one that ranks cards within a column).
/// Errors when an `After`/`Before` anchor isn't itself sequenced yet — with the
/// lazy default most members aren't, so the caller should sequence the anchor
/// first or use `First`/`Last`.
pub fn seq_rank(
    view: &BoardView,
    container: &Container,
    card: NoteId,
    position: &SeqPosition,
) -> std::result::Result<String, String> {
    let members = sequenced_members(view, container);
    let anchor_row = |anchor: &NoteId, offset: usize| {
        members
            .iter()
            .position(|m| &m.id == anchor)
            .map(|i| i + offset)
            .ok_or_else(|| {
                format!(
                    "{} isn't sequenced yet — sequence it first, or use --first/--last",
                    anchor.hex()
                )
            })
    };
    let to_row = match position {
        SeqPosition::First => 0,
        SeqPosition::Last => members.len(),
        SeqPosition::After(a) => anchor_row(a, 1)?,
        SeqPosition::Before(a) => anchor_row(a, 0)?,
    };
    Ok(rank_for_insert(
        &members,
        |m| m.id,
        |m| m.rank.as_str(),
        Some(card),
        to_row,
    ))
}

/// Slugify `name` into a column id not already present in `existing`.
fn unique_col_id(existing: &[ColumnDef], name: &str) -> String {
    let mut base: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    // Collapse runs of '-' and trim them from the ends.
    while base.contains("--") {
        base = base.replace("--", "-");
    }
    let base = base.trim_matches('-').to_string();
    let base = if base.is_empty() {
        "col".to_string()
    } else {
        base
    };

    let taken = |id: &str| existing.iter().any(|c| c.id == id);
    if !taken(&base) {
        return base;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if !taken(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Convenience re-export so the app layer can load a board without naming the
/// event module directly.
pub use event::load_board;

#[cfg(test)]
mod tests {
    use super::*;
    use enostr::FullKeypair;
    use futures_util::StreamExt;
    use nostrdb::{Config, Ndb, SubscriptionStream, Transaction};

    struct TestNdb {
        ndb: Ndb,
        _dir: tempfile::TempDir,
        kp: FullKeypair,
    }

    impl TestNdb {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
            Self {
                ndb,
                _dir: dir,
                kp: FullKeypair::generate(),
            }
        }

        fn secret(&self) -> [u8; 32] {
            self.kp.secret_key.secret_bytes()
        }

        /// Fold the board out of ndb until `pred` holds. Ingest is async, so
        /// between folds this awaits the writer's own subscription notification
        /// (see [`await_ingest`]) rather than polling against a wall-clock
        /// deadline — the only way to wait for an async ingest that a loaded CI
        /// runner can't race.
        async fn wait<F>(&self, pred: F) -> BoardView
        where
            F: Fn(&BoardView) -> bool,
        {
            let mut stream = ingest_stream(&self.ndb, &self.kp.pubkey);
            loop {
                {
                    let txn = Transaction::new(&self.ndb).unwrap();
                    if let Some(view) = load_board(&self.ndb, &txn, &self.kp.pubkey, BOARD_ID)
                        && pred(&view)
                    {
                        return view;
                    }
                }
                await_ingest(&mut stream).await;
            }
        }

        fn apply(&self, view: &BoardView, action: BoardAction) {
            super::apply(
                &self.ndb,
                BOARD_ID,
                view,
                &self.kp.pubkey,
                &Signer::new(&self.secret(), None),
                action,
                &mut NoPublish,
            );
        }
    }

    /// Open a live await-handle on `author`'s headway events. Subscribing before
    /// a wait means every note the async writer ingests *after* this point wakes
    /// [`await_ingest`], so the fold loops advance on the writer's own
    /// notification instead of a wall-clock sleep.
    fn ingest_stream(ndb: &Ndb, author: &Pubkey) -> SubscriptionStream {
        let sub = ndb.subscribe(&[event::headway_filter(author)]).unwrap();
        SubscriptionStream::new(ndb.clone(), sub)
    }

    /// Await the next batch of ingested notes on `stream`. Panics if the
    /// subscription closes first, so a predicate that never holds surfaces as a
    /// test-timeout hang rather than a silent spin.
    async fn await_ingest(stream: &mut SubscriptionStream) {
        stream
            .next()
            .await
            .expect("subscription closed before predicate held");
    }

    fn col_titles(view: &BoardView) -> Vec<String> {
        view.columns.iter().map(|c| c.name.clone()).collect()
    }

    fn card_titles(view: &BoardView, col: usize) -> Vec<String> {
        view.columns[col]
            .cards
            .iter()
            .map(|c| c.title.clone())
            .collect()
    }

    /// The id of the (first) card titled `title`, across every column.
    fn card_id_by_title(view: &BoardView, title: &str) -> Option<NoteId> {
        view.columns
            .iter()
            .flat_map(|c| c.cards.iter())
            .find(|c| c.title == title)
            .map(|c| c.id)
    }

    #[tokio::test]
    async fn reorder_subissues_promotes_unsequenced_children_into_exact_order() {
        let t = TestNdb::new();
        seed_demo(&t);
        let view = t.wait(|v| v.columns[1].cards.len() == 2).await;

        // A parent with three children, all left unsequenced (creation order).
        t.apply(
            &view,
            BoardAction::AddCard {
                col: 1,
                title: "epic".into(),
                labels: vec![],
                parent: None,
            },
        );
        let view = t.wait(|v| card_id_by_title(v, "epic").is_some()).await;
        let epic = card_id_by_title(&view, "epic").unwrap();
        for title in ["a", "b", "c"] {
            let v = t.wait(|v| card_id_by_title(v, "epic").is_some()).await;
            t.apply(
                &v,
                BoardAction::AddCard {
                    col: 1,
                    title: title.into(),
                    labels: vec![],
                    parent: Some(epic),
                },
            );
        }
        let view = t
            .wait(|v| find_card(v, epic).is_some_and(|c| c.subissues.len() == 3))
            .await;
        let id = |title: &str| card_id_by_title(&view, title).unwrap();
        let (a, b, c) = (id("a"), id("b"), id("c"));

        // Promote into an order that isn't creation order: c, a, b.
        t.apply(
            &view,
            BoardAction::ReorderSubissues {
                parent: epic,
                order: vec![c, a, b],
            },
        );

        // Every child is now sequenced, in exactly the requested order — the
        // proof a drop into an all-unsequenced list lands where dropped rather
        // than snapping to the top.
        let view = t
            .wait(|v| {
                find_card(v, epic)
                    .is_some_and(|card| card.subissues.iter().all(|s| s.seq.is_some()))
            })
            .await;
        let order: Vec<NoteId> = find_card(&view, epic)
            .unwrap()
            .subissues
            .iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(order, vec![c, a, b]);
    }

    /// Seed the populated demo board for the card-operation tests to act on.
    /// Columns: Backlog, Todo, In Progress, In Review, Done; cards 3 / 2 / 1 / 0 / 1.
    /// Seeded in the past so follow-up edits (stamped with the wall clock)
    /// always sort after it.
    fn seed_demo(t: &TestNdb) {
        seed_demo_board(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            BOARD_ID,
            1_700_000_000,
            &mut NoPublish,
        );
    }

    #[tokio::test]
    async fn seed_materialises_default_board() {
        let t = TestNdb::new();
        seed_default_board(&t.ndb, &t.kp.pubkey, &t.secret(), BOARD_ID, &mut NoPublish);

        // The default board is card-less: just the five columns.
        let view = t.wait(|v| v.columns.len() == 5).await;
        assert_eq!(
            col_titles(&view),
            ["Backlog", "Todo", "In Progress", "In Review", "Done"]
        );
        assert!(view.columns.iter().all(|c| c.cards.is_empty()));
    }

    #[tokio::test]
    async fn seed_demo_materialises_cards() {
        let t = TestNdb::new();
        seed_demo(&t);

        // seed_demo_board renames the first backlog card ("Nostr event model" →
        // "Define nostr event model for boards") via a subject edit ingested
        // *after* all seven cards land. Waiting only for the card count would
        // race that amendment, so wait for the renamed title itself — the
        // amendment folds after every card, so its presence also implies all
        // seven cards are here.
        let view = t
            .wait(|v| {
                v.columns[0]
                    .cards
                    .first()
                    .is_some_and(|c| c.title == "Define nostr event model for boards")
            })
            .await;
        assert_eq!(view.columns.iter().map(|c| c.cards.len()).sum::<usize>(), 7);
        assert_eq!(view.columns[0].cards.len(), 3);
        // Done is the last column; the seeded "done" card lands there.
        assert_eq!(view.columns.last().unwrap().cards.len(), 1);
        assert!(!view.columns[0].cards[0].description.is_empty());
    }

    #[tokio::test]
    async fn add_card_appends_to_column() {
        let t = TestNdb::new();
        seed_demo(&t);
        let view = t.wait(|v| v.columns[1].cards.len() == 2).await;

        t.apply(
            &view,
            BoardAction::AddCard {
                col: 1,
                title: "New idea".to_string(),
                labels: vec![],
                parent: None,
            },
        );

        let view = t.wait(|v| v.columns[1].cards.len() == 3).await;
        assert_eq!(card_titles(&view, 1).last().unwrap(), "New idea");
    }

    #[tokio::test]
    async fn add_card_with_labels_tags_the_new_card() {
        let t = TestNdb::new();
        seed_demo(&t);
        let view = t.wait(|v| v.columns[1].cards.len() == 2).await;

        t.apply(
            &view,
            BoardAction::AddCard {
                col: 1,
                title: "Tagged idea".to_string(),
                labels: vec!["bug".to_string(), "ux".to_string()],
                parent: None,
            },
        );

        let view = t
            .wait(|v| {
                v.columns[1]
                    .cards
                    .iter()
                    .any(|c| c.title == "Tagged idea" && c.labels.len() == 2)
            })
            .await;
        let card = view.columns[1]
            .cards
            .iter()
            .find(|c| c.title == "Tagged idea")
            .unwrap();
        assert_eq!(card.labels, vec!["bug".to_string(), "ux".to_string()]);
    }

    #[tokio::test]
    async fn block_and_unblock_edit_the_dependency_set() {
        let t = TestNdb::new();
        seed_demo(&t);
        let mut view = t.wait(|v| v.columns[1].cards.len() == 2).await;

        // Three fresh cards to wire edges between.
        for title in ["blocked", "dep one", "dep two"] {
            t.apply(
                &view,
                BoardAction::AddCard {
                    col: 0,
                    title: title.to_string(),
                    labels: vec![],
                    parent: None,
                },
            );
            view = t.wait(|v| card_id_by_title(v, title).is_some()).await;
        }
        let blocked = card_id_by_title(&view, "blocked").unwrap();
        let d1 = card_id_by_title(&view, "dep one").unwrap();
        let d2 = card_id_by_title(&view, "dep two").unwrap();

        // Block on two deps: the set accumulates rather than replacing.
        t.apply(
            &view,
            BoardAction::Block {
                card: blocked,
                on: d1,
            },
        );
        let view = t
            .wait(|v| {
                find_card(v, blocked).is_some_and(|c| c.blocked_by.iter().any(|e| e.id == d1))
            })
            .await;
        t.apply(
            &view,
            BoardAction::Block {
                card: blocked,
                on: d2,
            },
        );
        let view = t
            .wait(|v| find_card(v, blocked).is_some_and(|c| c.blocked_by.len() == 2))
            .await;

        let card = find_card(&view, blocked).unwrap();
        assert!(card.is_blocked());
        let ids: Vec<NoteId> = card.blocked_by.iter().map(|e| e.id).collect();
        assert!(ids.contains(&d1) && ids.contains(&d2));
        // The reverse edge resolves on the blocker.
        assert_eq!(
            find_card(&view, d1)
                .unwrap()
                .blocks
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>(),
            vec![blocked]
        );

        // Cycle guard: blocking d1 on `blocked` would close a loop, so it's
        // refused and d1 stays free (the set above is unchanged).
        t.apply(
            &view,
            BoardAction::Block {
                card: d1,
                on: blocked,
            },
        );
        let view = t
            .wait(|v| find_card(v, blocked).is_some_and(|c| c.blocked_by.len() == 2))
            .await;
        assert!(find_card(&view, d1).unwrap().blocked_by.is_empty());

        // Unblock one dep: the other survives (the edit rebuilds from the raw set,
        // so removing d1 doesn't drop d2).
        t.apply(
            &view,
            BoardAction::Unblock {
                card: blocked,
                on: d1,
            },
        );
        let view = t
            .wait(|v| find_card(v, blocked).is_some_and(|c| c.blocked_by.len() == 1))
            .await;
        assert_eq!(find_card(&view, blocked).unwrap().blocked_by[0].id, d2);
    }

    #[tokio::test]
    async fn publisher_receives_a_frame_per_ingested_event() {
        #[derive(Default)]
        struct Collect(Vec<String>);
        impl Publisher for Collect {
            fn publish(&mut self, frame: &str) {
                self.0.push(frame.to_string());
            }
        }

        let t = TestNdb::new();
        seed_demo(&t);
        let view = t.wait(|v| v.columns[1].cards.len() == 2).await;

        // AddCard ingests two events — the issue and its placement — so the
        // publisher should see exactly two ready-to-send EVENT frames.
        let mut sink = Collect::default();
        super::apply(
            &t.ndb,
            BOARD_ID,
            &view,
            &t.kp.pubkey,
            &Signer::new(&t.secret(), None),
            BoardAction::AddCard {
                col: 1,
                title: "Tracked".to_string(),
                labels: vec![],
                parent: None,
            },
            &mut sink,
        );

        assert_eq!(sink.0.len(), 2, "issue + placement each publish a frame");
        for frame in &sink.0 {
            assert!(
                frame.starts_with("[\"EVENT\","),
                "frame is a NIP-01 EVENT message: {frame}"
            );
        }
    }

    #[tokio::test]
    async fn move_card_changes_column() {
        let t = TestNdb::new();
        seed_demo(&t);
        let view = t.wait(|v| v.columns[0].cards.len() == 3).await;

        // Move a Backlog card into Done (the last column, which seeds one card).
        let done = view.columns.len() - 1;
        let card = view.columns[0].cards[0].id;
        t.apply(
            &view,
            BoardAction::MoveCard {
                card,
                to_col: done,
                to_row: view.columns[done].cards.len(),
            },
        );

        let view = t.wait(|v| v.columns[done].cards.len() == 2).await;
        assert_eq!(view.columns[0].cards.len(), 2);
        assert!(view.columns[done].cards.iter().any(|c| c.id == card));
    }

    #[tokio::test]
    async fn edit_title_description_and_labels() {
        let t = TestNdb::new();
        seed_demo(&t);
        let view = t.wait(|v| v.columns[1].cards.len() == 2).await;
        // The second Todo card ("Column reordering") is seeded without labels,
        // so the SetLabels union below is exactly the two we add.
        let card = view.columns[1].cards[1].id;

        t.apply(
            &view,
            BoardAction::EditTitle {
                card,
                title: "Renamed".to_string(),
            },
        );
        t.apply(
            &view,
            BoardAction::EditDescription {
                card,
                description: "the details".to_string(),
            },
        );
        t.apply(
            &view,
            BoardAction::SetLabels {
                card,
                labels: vec!["bug".to_string(), "ux".to_string()],
            },
        );

        let view = t
            .wait(|v| {
                v.columns[1].cards.iter().any(|c| {
                    c.id == card
                        && c.title == "Renamed"
                        && c.description == "the details"
                        && c.labels.len() == 2
                })
            })
            .await;
        let edited = view.columns[1].cards.iter().find(|c| c.id == card).unwrap();
        assert_eq!(edited.title, "Renamed");
        assert_eq!(edited.description, "the details");
        assert_eq!(edited.labels, vec!["bug".to_string(), "ux".to_string()]);
    }

    #[tokio::test]
    async fn add_comment_and_reply_fold_onto_the_card() {
        let t = TestNdb::new();
        seed_demo(&t);
        let view = t.wait(|v| v.columns[1].cards.len() == 2).await;
        let card = view.columns[1].cards[0].id;

        // Top-level comment.
        t.apply(
            &view,
            BoardAction::AddComment {
                card,
                body: "first comment".to_string(),
                reply_to: None,
            },
        );
        let view = t
            .wait(|v| {
                v.columns[1]
                    .cards
                    .iter()
                    .any(|c| c.id == card && c.comments.len() == 1)
            })
            .await;
        let parent = view.columns[1]
            .cards
            .iter()
            .find(|c| c.id == card)
            .unwrap()
            .comments[0]
            .id;

        // A reply threaded under that comment.
        t.apply(
            &view,
            BoardAction::AddComment {
                card,
                body: "a reply".to_string(),
                reply_to: Some(parent),
            },
        );
        let view = t
            .wait(|v| {
                v.columns[1]
                    .cards
                    .iter()
                    .any(|c| c.id == card && c.comments.len() == 2)
            })
            .await;

        let comments = &view.columns[1]
            .cards
            .iter()
            .find(|c| c.id == card)
            .unwrap()
            .comments;
        assert_eq!(comments[0].body, "first comment");
        assert_eq!(comments[0].parent, None);
        assert_eq!(comments[1].body, "a reply");
        assert_eq!(comments[1].parent, Some(parent));
    }

    #[tokio::test]
    async fn delete_card_removes_it() {
        let t = TestNdb::new();
        seed_demo(&t);
        let view = t.wait(|v| v.columns[0].cards.len() == 3).await;
        let card = view.columns[0].cards[0].id;

        t.apply(&view, BoardAction::DeleteCard { card });

        let view = t.wait(|v| v.columns[0].cards.len() == 2).await;
        assert!(!view.columns[0].cards.iter().any(|c| c.id == card));
    }

    #[tokio::test]
    async fn archive_then_restore_round_trips_to_origin() {
        let t = TestNdb::new();
        seed_demo(&t);
        // Pick a card out of "In Progress" (column 2), not the first column, so a
        // restore that ignored the origin would land it somewhere else.
        let view = t.wait(|v| v.columns[2].cards.len() == 1).await;
        let card = view.columns[2].cards[0].id;

        t.apply(&view, BoardAction::ArchiveCard { card });

        // It leaves the columns and shows up in the archived list, with origin.
        let view = t.wait(|v| !v.archived.is_empty()).await;
        assert!(
            view.columns
                .iter()
                .all(|c| c.cards.iter().all(|c| c.id != card))
        );
        assert_eq!(view.archived.len(), 1);
        assert_eq!(view.archived[0].card.id, card);
        assert_eq!(view.archived[0].from.as_deref(), Some("in-progress"));

        t.apply(&view, BoardAction::RestoreCard { card });

        // Restored back into the exact column it came from, and unarchived.
        let view = t
            .wait(|v| v.archived.is_empty() && v.columns[2].cards.len() == 1)
            .await;
        assert_eq!(view.columns[2].cards[0].id, card);
    }

    #[tokio::test]
    async fn column_ops_round_trip() {
        let t = TestNdb::new();
        seed_demo(&t);
        let view = t.wait(|v| v.columns.len() == 5).await;

        t.apply(
            &view,
            BoardAction::AddColumn {
                name: "Review".to_string(),
            },
        );
        let view = t.wait(|v| v.columns.len() == 6).await;
        assert_eq!(view.columns[5].name, "Review");

        t.apply(
            &view,
            BoardAction::RenameColumn {
                col: 0,
                name: "Inbox".to_string(),
            },
        );
        let view = t.wait(|v| v.columns[0].name == "Inbox").await;

        t.apply(&view, BoardAction::MoveColumn { from: 0, to: 1 });
        let view = t.wait(|v| v.columns[1].name == "Inbox").await;

        t.apply(&view, BoardAction::RemoveColumn { col: 1 });
        let view = t
            .wait(|v| !v.columns.iter().any(|c| c.name == "Inbox"))
            .await;
        // The removed column's cards aren't lost; they fall back to column 0.
        assert!(view.columns.iter().map(|c| c.cards.len()).sum::<usize>() >= 7);
    }

    #[tokio::test]
    async fn rename_board_changes_title_preserving_columns_and_cards() {
        let t = TestNdb::new();
        seed_demo(&t);
        let view = t
            .wait(|v| v.columns.iter().map(|c| c.cards.len()).sum::<usize>() == 7)
            .await;
        let cols_before = col_titles(&view);

        t.apply(
            &view,
            BoardAction::RenameBoard {
                title: "Renamed Board".to_string(),
            },
        );

        let view = t.wait(|v| v.title == "Renamed Board").await;
        // Slug (the addressable `d`-tag) is untouched, so refs still resolve.
        assert_eq!(view.id, BOARD_ID);
        // Columns and cards ride along the republished definition unchanged.
        assert_eq!(col_titles(&view), cols_before);
        assert_eq!(view.columns.iter().map(|c| c.cards.len()).sum::<usize>(), 7);
    }

    #[test]
    fn board_slug_normalizes_titles() {
        let free = |_: &str| false;
        assert_eq!(board_slug("Work", free), "work");
        assert_eq!(board_slug("My Work Board", free), "my-work-board");
        assert_eq!(board_slug("  Spaced  Out  ", free), "spaced-out");
        assert_eq!(board_slug("C++ & Rust!", free), "c-rust");
        assert_eq!(board_slug("2024 Goals", free), "2024-goals");
    }

    #[test]
    fn board_slug_falls_back_when_empty() {
        let free = |_: &str| false;
        assert_eq!(board_slug("", free), "board");
        assert_eq!(board_slug("   ", free), "board");
        assert_eq!(board_slug("!@#$", free), "board");
    }

    #[test]
    fn board_slug_disambiguates_collisions() {
        // "work" and "work-2" are taken; the next free slug is "work-3".
        let taken = |s: &str| matches!(s, "work" | "work-2");
        assert_eq!(board_slug("Work", taken), "work-3");
        // The fallback also disambiguates.
        let taken_board = |s: &str| s == "board";
        assert_eq!(board_slug("", taken_board), "board-2");
    }

    /// Load an arbitrary board (the [`TestNdb`] helpers are pinned to `BOARD_ID`),
    /// awaiting the writer's ingest notifications until `pred` holds.
    async fn poll_board(
        t: &TestNdb,
        board_id: &str,
        pred: impl Fn(&BoardView) -> bool,
    ) -> BoardView {
        let mut stream = ingest_stream(&t.ndb, &t.kp.pubkey);
        loop {
            {
                let txn = Transaction::new(&t.ndb).unwrap();
                if let Some(view) = load_board(&t.ndb, &txn, &t.kp.pubkey, board_id)
                    && pred(&view)
                {
                    return view;
                }
            }
            await_ingest(&mut stream).await;
        }
    }

    /// Seed two boards, add a card to one, and add a card we can relocate.
    async fn two_boards_with_a_card(t: &TestNdb) -> NoteId {
        seed_default_board(&t.ndb, &t.kp.pubkey, &t.secret(), "src", &mut NoPublish);
        seed_default_board(&t.ndb, &t.kp.pubkey, &t.secret(), "dst", &mut NoPublish);
        poll_board(t, "src", |v| v.columns.len() == 5).await;
        poll_board(t, "dst", |v| v.columns.len() == 5).await;

        let src = poll_board(t, "src", |v| v.columns.len() == 5).await;
        super::apply(
            &t.ndb,
            "src",
            &src,
            &t.kp.pubkey,
            &Signer::new(&t.secret(), None),
            BoardAction::AddCard {
                col: 0,
                title: "Roamer".to_string(),
                labels: vec!["wandering".to_string()],
                parent: None,
            },
            &mut NoPublish,
        );
        poll_board(t, "src", |v| v.columns[0].cards.len() == 1)
            .await
            .columns[0]
            .cards[0]
            .id
    }

    #[tokio::test]
    async fn link_card_places_on_both_boards() {
        let t = TestNdb::new();
        let card = two_boards_with_a_card(&t).await;

        let src = poll_board(&t, "src", |v| v.columns[0].cards.len() == 1).await;
        let dst = poll_board(&t, "dst", |v| v.columns.len() == 5).await;
        link_card(
            &t.ndb,
            BoardRef {
                id: "src",
                view: &src,
            },
            BoardRef {
                id: "dst",
                view: &dst,
            },
            &Signer::new(&t.secret(), None),
            card,
            &mut NoPublish,
        );

        // Same card on both boards, with its labels intact (it's shared, not copied).
        let src = poll_board(&t, "src", |v| v.columns[0].cards.len() == 1).await;
        let dst = poll_board(&t, "dst", |v| v.columns[0].cards.len() == 1).await;
        assert_eq!(src.columns[0].cards[0].id, card);
        assert_eq!(dst.columns[0].cards[0].id, card);
        assert_eq!(
            dst.columns[0].cards[0].labels,
            vec!["wandering".to_string()]
        );
    }

    #[tokio::test]
    async fn move_card_between_boards_relocates_it() {
        let t = TestNdb::new();
        let card = two_boards_with_a_card(&t).await;

        let src = poll_board(&t, "src", |v| v.columns[0].cards.len() == 1).await;
        let dst = poll_board(&t, "dst", |v| v.columns.len() == 5).await;
        move_card_between_boards(
            &t.ndb,
            BoardRef {
                id: "src",
                view: &src,
            },
            BoardRef {
                id: "dst",
                view: &dst,
            },
            &Signer::new(&t.secret(), None),
            card,
            &mut NoPublish,
        );

        // Leaves src, lands on dst — same id, same overlays.
        let dst = poll_board(&t, "dst", |v| v.columns[0].cards.len() == 1).await;
        poll_board(&t, "src", |v| v.columns[0].cards.is_empty()).await;
        assert_eq!(dst.columns[0].cards[0].id, card);
        assert_eq!(dst.columns[0].cards[0].title, "Roamer");
    }

    #[tokio::test]
    async fn move_card_preserves_column_when_target_has_it() {
        let t = TestNdb::new();
        let card = two_boards_with_a_card(&t).await;

        // Push the card into In Progress on src (both default boards share this column).
        let src = poll_board(&t, "src", |v| v.columns[0].cards.len() == 1).await;
        super::apply(
            &t.ndb,
            "src",
            &src,
            &t.kp.pubkey,
            &Signer::new(&t.secret(), None),
            BoardAction::MoveCard {
                card,
                to_col: 2, // in-progress
                to_row: 0,
            },
            &mut NoPublish,
        );

        let src = poll_board(&t, "src", |v| v.columns[2].cards.len() == 1).await;
        let dst = poll_board(&t, "dst", |v| v.columns.len() == 5).await;
        move_card_between_boards(
            &t.ndb,
            BoardRef {
                id: "src",
                view: &src,
            },
            BoardRef {
                id: "dst",
                view: &dst,
            },
            &Signer::new(&t.secret(), None),
            card,
            &mut NoPublish,
        );

        // Lands in the same-id column (In Progress), not the first column.
        let dst = poll_board(&t, "dst", |v| v.columns[2].cards.len() == 1).await;
        assert_eq!(dst.columns[2].cards[0].id, card);
        assert!(dst.columns[0].cards.is_empty());
    }

    #[tokio::test]
    async fn move_card_falls_back_to_first_column_when_target_lacks_it() {
        let t = TestNdb::new();
        seed_default_board(&t.ndb, &t.kp.pubkey, &t.secret(), "src", &mut NoPublish);
        // A target board whose columns don't include "in-progress".
        ingest(
            &t.ndb,
            build_board(
                "dst",
                "Slim",
                "",
                &[
                    ColumnDef::new("inbox", "Inbox"),
                    ColumnDef::new("done", "Done"),
                ],
            ),
            &t.secret(),
            &mut NoPublish,
        );
        poll_board(&t, "src", |v| v.columns.len() == 5).await;
        poll_board(&t, "dst", |v| v.columns.len() == 2).await;

        // Add a card and move it into In Progress on src.
        let src = poll_board(&t, "src", |v| v.columns.len() == 5).await;
        super::apply(
            &t.ndb,
            "src",
            &src,
            &t.kp.pubkey,
            &Signer::new(&t.secret(), None),
            BoardAction::AddCard {
                col: 2, // in-progress
                title: "Homeless".to_string(),
                labels: vec![],
                parent: None,
            },
            &mut NoPublish,
        );

        let src = poll_board(&t, "src", |v| v.columns[2].cards.len() == 1).await;
        let card = src.columns[2].cards[0].id;
        let dst = poll_board(&t, "dst", |v| v.columns.len() == 2).await;
        move_card_between_boards(
            &t.ndb,
            BoardRef {
                id: "src",
                view: &src,
            },
            BoardRef {
                id: "dst",
                view: &dst,
            },
            &Signer::new(&t.secret(), None),
            card,
            &mut NoPublish,
        );

        // No "in-progress" on dst, so it falls back to the first column (Inbox).
        let dst = poll_board(&t, "dst", |v| v.columns[0].cards.len() == 1).await;
        assert_eq!(dst.columns[0].id, "inbox");
        assert_eq!(dst.columns[0].cards[0].id, card);
    }

    /// An edit applied with a [`Signer::shared`] channel is sealed into a kind-1081
    /// SNS envelope before it is ingested and published: nostrdb auto-unwraps it
    /// (the team_root is registered), so the card surfaces through the *shared*
    /// multi-writer read fold and the stored issue is an unwrapped rumor — the
    /// proof it went out sealed rather than as a plaintext event.
    #[tokio::test]
    async fn shared_board_edit_round_trips_as_sns() {
        let t = TestNdb::new();
        // Register the team root so ndb peels our own envelopes on local ingest.
        let mut root = [0u8; 32];
        root[0] = 0x11;
        root[31] = 0x22;
        assert!(t.ndb.add_team_root(&root));
        let channel = SnsChannel {
            keys: enostr::sns::derive_sns_keys(&root).expect("keys"),
        };

        // A shared board's definition travels sealed too — members subscribe to no
        // plaintext leg, and the shared fold only gathers team-sealed rumors — so
        // seal the definition into the channel rather than seeding it plaintext.
        ingest_signed(
            &t.ndb,
            build_board(BOARD_ID, "Headway", "", &default_columns()),
            &Signer::shared(&t.secret(), &channel),
            &mut NoPublish,
        );
        let view = t.wait(|v| v.id == BOARD_ID).await;
        super::apply(
            &t.ndb,
            BOARD_ID,
            &view,
            &t.kp.pubkey,
            &Signer::new(&t.secret(), Some(&channel)),
            BoardAction::AddCard {
                col: 0,
                title: "Sealed card".to_string(),
                labels: vec![],
                parent: None,
            },
            &mut NoPublish,
        );

        // The card only surfaces if the envelope unwrapped and the shared fold
        // gathered the resulting rumor.
        let addr = board_address(&t.kp.pubkey, BOARD_ID);
        let card_id = wait_shared_card(
            &t.ndb,
            &t.kp.pubkey,
            &addr,
            &channel.keys.team_keypair.pubkey,
            "Sealed card",
        )
        .await;

        let txn = Transaction::new(&t.ndb).unwrap();
        let issue = t.ndb.get_note_by_id(&txn, card_id.bytes()).unwrap();
        assert!(
            issue.is_rumor(),
            "a shared-board edit must be stored as an unwrapped rumor, not plaintext"
        );
    }

    /// Fold the *shared* board (multi-writer) until a card titled `title` appears,
    /// returning its id. Mirrors [`TestNdb::wait`] but over
    /// [`event::fold_shared_board`], awaiting the writer's ingest notification
    /// between folds rather than sleeping.
    async fn wait_shared_card(
        ndb: &Ndb,
        author: &Pubkey,
        addr: &str,
        team_pubkey: &Pubkey,
        title: &str,
    ) -> NoteId {
        let mut stream = ingest_stream(ndb, author);
        loop {
            {
                let txn = Transaction::new(ndb).unwrap();
                if let Some(reducer) = event::fold_shared_board(ndb, &txn, addr, team_pubkey) {
                    let found = reducer
                        .finalize()
                        .iter()
                        .flat_map(|b| b.columns.iter())
                        .flat_map(|c| c.cards.iter())
                        .find(|c| c.title == title)
                        .map(|c| c.id);
                    if let Some(id) = found {
                        return id;
                    }
                }
            }
            await_ingest(&mut stream).await;
        }
    }

    /// Round-trip the board-selection preference through nostrdb: a save is
    /// PNS-wrapped + ingested, and [`event::load_board_pref`] reads the coordinate
    /// back; a later save supersedes latest-wins. Registers the device key with
    /// `add_key` so nostrdb unwraps the kind-1080 envelope (the app does this at
    /// sign-in), and subscribes to the inner kind-30623 so the fold advances on
    /// the writer's ingest notification rather than a sleep.
    #[tokio::test]
    async fn board_pref_round_trip_latest_wins() {
        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
        let kp = FullKeypair::generate();
        let secret = kp.secret_key.secret_bytes();
        assert!(ndb.add_key(&secret));

        let pref_filter = nostrdb::Filter::new()
            .authors([kp.pubkey.bytes()])
            .kinds([event::KIND_BOARD_PREF as u64])
            .build();
        let sub = ndb.subscribe(&[pref_filter]).unwrap();
        let mut stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);

        // Advance the ingest until the preference reads back as `want`. Checks
        // first so an already-committed save doesn't hang awaiting a note that
        // won't come.
        async fn wait_pref(
            ndb: &Ndb,
            stream: &mut SubscriptionStream,
            author: &Pubkey,
            want: &event::BoardCoord,
        ) {
            while event::load_board_pref(ndb, author).as_ref() != Some(want) {
                stream.next().await.expect("subscription open");
            }
        }

        let work = event::BoardCoord::new(*kp.pubkey.bytes(), "work");
        let personal = event::BoardCoord::new(*kp.pubkey.bytes(), "personal");

        // Nothing saved yet.
        assert_eq!(event::load_board_pref(&ndb, &kp.pubkey), None);

        save_board_pref(&ndb, &kp.pubkey, &secret, &work, &mut NoPublish);
        wait_pref(&ndb, &mut stream, &kp.pubkey, &work).await;

        // A later save supersedes the previous revision latest-wins.
        save_board_pref(&ndb, &kp.pubkey, &secret, &personal, &mut NoPublish);
        wait_pref(&ndb, &mut stream, &kp.pubkey, &personal).await;
    }

    /// A preference note written before selection became coordinate-aware stored
    /// the bare slug as its content. [`event::load_board_pref`] must still resolve
    /// it — as an own board (`owner = the querying author`) — so a previously saved
    /// selection keeps working without a migration.
    #[tokio::test]
    async fn board_pref_reads_legacy_bare_slug() {
        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
        let kp = FullKeypair::generate();
        let secret = kp.secret_key.secret_bytes();
        assert!(ndb.add_key(&secret));

        let pref_filter = nostrdb::Filter::new()
            .authors([kp.pubkey.bytes()])
            .kinds([event::KIND_BOARD_PREF as u64])
            .build();
        let sub = ndb.subscribe(&[pref_filter]).unwrap();
        let mut stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);

        // Hand-write the legacy note: its content is a bare slug, not a coordinate.
        let inner = event::build_board_pref("work")
            .created_at(1)
            .sign(&secret)
            .build()
            .expect("build legacy pref");
        ingest_pns(&ndb, &inner, &secret, &mut NoPublish);

        // It resolves to an own-board coordinate.
        let want = event::BoardCoord::new(*kp.pubkey.bytes(), "work");
        while event::load_board_pref(&ndb, &kp.pubkey).as_ref() != Some(&want) {
            stream.next().await.expect("subscription open");
        }
    }
}
