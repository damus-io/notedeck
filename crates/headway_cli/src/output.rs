//! Printing: the board listing, `show`'s card detail, `next`'s frontier, the
//! `board` list and `seq`'s place, each as text or `--json`. The text comes
//! from [`headway::render`], which the app's tools render with too, so `nd
//! headway show` prints what `headway show` prints.

use std::io::IsTerminal;

use nostrdb_net::{NoteId, Pubkey};
use serde_json::json;

use headway::event::{self, BoardView, Container};
use headway::render;

use nostrdb_net::relay::sync::Result;

pub(crate) use headway::render::{card_count, declined_message, deleted_board_error, plain_ref};

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
/// as stored (see [`render::review_source`]), so a script can `>
/// book/<word-id>.md`.
pub(crate) fn print_review_source(view: &BoardView, sel: &str, name: &str) -> Result<()> {
    use std::io::Write;
    let id = event::resolve_card(view, sel)?;
    let card = render::find_card(view, &id).ok_or_else(|| format!("no card matching '{sel}'"))?;
    let text = render::review_source(card.card, name)?;
    std::io::stdout().write_all(text.as_bytes())?;
    Ok(())
}

/// Print the work-order frontier for `next` (see [`render::next_cards`]):
/// each card as a `headway:board/word-id` ref an agent can paste straight into
/// `move`/`show`, or in JSON mode an array of `{ref, id, title}` objects. An
/// empty frontier says so on stderr and prints nothing to stdout.
pub(crate) fn print_next(
    view: &BoardView,
    container: &Container,
    ready: bool,
    limit: Option<usize>,
    labels: &[String],
    as_json: bool,
) {
    let picked = render::next_cards(view, container, labels, ready, limit);
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&picked).unwrap_or_else(|_| "[]".into())
        );
        return;
    }
    let color = color();
    if picked.is_empty() {
        eprintln!("{}", render::dim(color, render::NOTHING_READY));
    }
    print!("{}", render::next_lines(&picked, color));
}

/// Print where `card` sits in `container`'s work-order for `seq <card>` with no
/// position flag (see [`render::seq_place`]). Errors when the walk never
/// reaches the card.
pub(crate) fn print_seq_place(
    view: &BoardView,
    container: &Container,
    card: NoteId,
    as_json: bool,
) -> Result<()> {
    let place = render::seq_place(view, container, card)?;
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&place).unwrap_or_else(|_| "null".into())
        );
    } else {
        print!("{}", place.text(color()));
    }
    Ok(())
}
