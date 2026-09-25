//! Resolving boards and cards out of a folded reducer or a finalized
//! [`BoardView`]: pick a board or card, find a card's column position or the
//! board it lives on, and resolve a user's card selector (word id, hex prefix).

use nostrdb_net::{NoteId, Pubkey};

use super::reduce::BoardReducer;
use super::view::{BoardView, CardView};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::parse::tests::note_id;
    use nostrdb_net::FullKeypair;

    use nostrdb::NoteBuilder;

    use crate::event::build::{
        build_board, build_issue, build_labels, build_placement, build_subject_edit,
    };
    use crate::event::model::{COL_DELETED, ColumnDef, Priority, board_address};
    use crate::event::parse::parse;
    use crate::event::view::ColumnView;

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
