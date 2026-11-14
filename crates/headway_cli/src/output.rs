//! Printing: the board listing, `show`'s card detail, `next`'s frontier, the
//! `board` list and `seq`'s place, each as text or `--json`. The text comes
//! from [`headway::render`], which the app's tools render with too, so `nd
//! headway show` prints what `headway show` prints.

use std::io::IsTerminal;

use nostrdb_net::{NoteId, Pubkey};
use serde_json::json;

use headway::event::{self, BoardView, Container};
use headway::render;
use headway::traversal::{self, NotReady};

use nostrdb_net::relay::sync::Result;

pub(crate) use headway::render::{
    BLOCKED_GLYPH, PlacedCard, card_count, declined_message, deleted_board_error, labels_suffix,
    placed_cards, plain_ref, priority_glyph,
};

/// Whether text goes out colored: when stdout is a terminal, as the CLI has
/// always decided.
fn color() -> bool {
    std::io::stdout().is_terminal()
}

/// Print the board list (see [`render::boards`]).
pub(crate) fn print_boards(own: &[BoardView], shared: &[BoardView], current: &str) {
    print!("{}", render::boards(own, shared, current, color()));
}

/// Print the tail of `headway board`: the deleted boards (see
/// [`render::deleted_boards`]).
pub(crate) fn print_deleted_boards(deleted: &[BoardView]) {
    print!("{}", render::deleted_boards(deleted, color()));
}

/// Print one board, as [`render::board`] or its `--json`.
pub(crate) fn print_board(view: &BoardView, as_json: bool, show_archived: bool) {
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&event::board_json(view))
                .unwrap_or_else(|_| "null".into())
        );
        return;
    }
    print!("{}", render::board(view, show_archived, color()));
}

/// Print every board in the cache. In text mode see [`render::all_boards`]; in
/// JSON mode they become a single array so the combined output stays
/// machine-parseable, every board carrying an `owner` hex, since slugs are
/// only unique per owner.
pub(crate) fn print_all_boards(
    boards: &[BoardView],
    me: &Pubkey,
    as_json: bool,
    show_archived: bool,
) {
    if as_json {
        let arr: Vec<_> = boards
            .iter()
            .map(|view| {
                let mut board = event::board_json(view);
                board["owner"] = json!(hex::encode(view.author));
                board
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&arr).unwrap_or_else(|_| "[]".into())
        );
        return;
    }
    print!(
        "{}",
        render::all_boards(boards, me.bytes(), show_archived, color())
    );
}

/// Print only the cards named by `sels` (each a card id or unique short prefix).
/// In JSON mode this is [`render::cards_json`]; otherwise [`render::card_details`].
pub(crate) fn print_cards(view: &BoardView, sels: &[String], as_json: bool) -> Result<()> {
    let cards = render::resolve_cards(view, sels)?;
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&render::cards_json(view, &cards))
                .unwrap_or_else(|_| "null".into())
        );
    } else {
        print!("{}", render::card_details(view, &cards, color()));
    }
    Ok(())
}

/// Print the text of the review source named `name` on card `sel`, exactly
/// as stored, so a script can `> book/<word-id>.md`. The newest record that
/// carries a file of that name wins, so a later record without sources doesn't
/// hide the explainer an earlier one stored.
pub(crate) fn print_review_source(view: &BoardView, sel: &str, name: &str) -> Result<()> {
    use std::io::Write;
    let id = event::resolve_card(view, sel)?;
    let card = render::find_card(view, &id).ok_or_else(|| format!("no card matching '{sel}'"))?;
    let source = card
        .card
        .reviews
        .iter()
        .find_map(|r| r.sources.iter().find(|s| s.name == name))
        .ok_or_else(|| format!("no review source named '{name}' on {sel}"))?;
    std::io::stdout().write_all(source.text.as_bytes())?;
    Ok(())
}

/// Print the work-order frontier for `next`: the ready cards of `container`, each
/// as a `headway:board/word-id` ref an agent can paste straight into `move`/`show`.
///
/// Which: `labels` (from `--label`) keeps only cards carrying every one of them,
/// applied to the frontier before the count, so `--label x -n 1` is the first
/// ready `x` card rather than the first ready card if it happens to be `x`.
///
/// How many: `-n <k>` caps to `k`; otherwise `--ready` prints the whole ready set
/// (the parallel-dispatch frontier) and the default prints just the single next
/// card. In JSON mode it's an array of `{ref, id, title}` objects.
pub(crate) fn print_next(
    view: &BoardView,
    container: &Container,
    ready: bool,
    limit: Option<usize>,
    labels: &[String],
    as_json: bool,
) {
    let frontier = traversal::ready_labelled(view, container, labels);
    let count = match (ready, limit) {
        (_, Some(k)) => k,
        (true, None) => frontier.len(),
        (false, None) => 1,
    };
    let picked = frontier.into_iter().take(count);

    if as_json {
        let arr: Vec<_> = picked
            .map(|c| {
                json!({
                    "ref": plain_ref(view, &c.id),
                    "id": c.id.hex(),
                    "title": c.title,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&arr).unwrap_or_else(|_| "[]".into())
        );
        return;
    }

    let color = color();
    let mut any = false;
    for c in picked {
        any = true;
        print!(
            "{}",
            render::ready_line(&plain_ref(view, &c.id), &c.title, color)
        );
    }
    if !any {
        eprintln!("{}", render::dim(color, render::NOTHING_READY));
    }
}

/// Print where `card` sits in `container`'s work-order for `seq <card>` with no
/// position flag: its index in the whole walk, its neighbours, whether it has
/// a rank of its own, and how much of the ready frontier is ahead of it.
///
/// Both counts are printed because they answer different questions: "31 of
/// 40" is the place in the order, "22 ready ahead" is how many cards `next`
/// would hand out first. Errors when the walk never reaches the card.
pub(crate) fn print_seq_place(
    view: &BoardView,
    container: &Container,
    card: NoteId,
    as_json: bool,
) -> Result<()> {
    let container_ref = match container {
        Container::BoardRoot(slug) => slug.clone(),
        Container::Card(id) => plain_ref(view, &NoteId::new(*id)),
    };
    let in_name = match container {
        Container::BoardRoot(_) => format!("{container_ref} (board root)"),
        Container::Card(_) => container_ref.clone(),
    };
    let place = traversal::place(view, container, card).ok_or_else(|| {
        format!(
            "{} is not in the work order of {in_name}",
            plain_ref(view, &card)
        )
    })?;
    let title = |id: NoteId| view.card(id).map_or("", |c| c.title.as_str());
    let why = place.not_ready.map(|r| match r {
        NotReady::Done => "done".to_string(),
        NotReady::InProgress => "In Progress".to_string(),
        NotReady::Blocked { by } => format!("blocked by {}", plain_ref(view, &by)),
        NotReady::OpenSubissues => "has open subissues".to_string(),
    });

    if as_json {
        let neighbour = |id: Option<NoteId>| {
            id.map(|id| json!({ "ref": plain_ref(view, &id), "title": title(id) }))
        };
        let obj = json!({
            "ref": plain_ref(view, &card),
            "container": container_ref,
            "index": place.index,
            "of": place.of,
            "sequenced": place.sequenced,
            "before": neighbour(place.before),
            "after": neighbour(place.after),
            "ready_ahead": place.ready_ahead,
            "ready": place.ready(),
            "not_ready_why": why,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&obj).unwrap_or_else(|_| "null".into())
        );
        return Ok(());
    }

    let dim = nostrdb_net::relay::sync::dim;
    let neighbour = |id: Option<NoteId>, end: &str| match id {
        Some(id) => format!("{}  {}", plain_ref(view, &id), dim(title(id))),
        None => dim(end),
    };
    println!("{}  {}", plain_ref(view, &card), dim(title(card)));
    println!("  order   {} of {} in {in_name}", place.index, place.of);
    println!(
        "  rank    {}",
        if place.sequenced {
            "sequenced"
        } else {
            "unsequenced: placed by when it was made"
        }
    );
    println!("  before  {}", neighbour(place.before, "(first)"));
    println!("  after   {}", neighbour(place.after, "(last)"));
    println!(
        "  ready   {} ready ahead; {}",
        place.ready_ahead,
        match &why {
            None => "this one is ready".to_string(),
            Some(why) => format!("not ready: {why}"),
        }
    );
    Ok(())
}
