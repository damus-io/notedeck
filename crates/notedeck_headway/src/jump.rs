//! Headway's half of the chrome's jump list (vim's Ctrl-O / Ctrl-I): what a
//! location in Headway is, and putting the view back at one.
//!
//! The chrome records a location before each keymap binding marked `jump`:
//! `g g` and `G` on the grid, the issue list and a scrolling pane, and the
//! detail's `n` / `p`. A location is where the keyboard is within the views,
//! not which view is open: the board, card or list in front is route state,
//! which Alt-←/→ walk.

use std::rc::Rc;

use nostrdb_net::NoteId;
use notedeck::{JumpLocation, JumpTarget};

use crate::Headway;
use crate::ui::{DetailScroll, DetailSpot, ListCursor};

/// Where Headway's keyboard is: each view's cursor, and how far down the
/// open card's detail is scrolled.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HeadwayJump {
    /// The board grid's cursor.
    grid: Option<NoteId>,
    /// The issue list's cursor: a card's row or a group's header.
    list: Option<ListCursor>,
    /// The open card's detail scroll, while a card is open.
    detail: Option<DetailSpot>,
}

impl Headway {
    /// Where the keyboard is now.
    pub(crate) fn jump_here(&self) -> HeadwayJump {
        HeadwayJump {
            grid: self.state.cursor(),
            list: self.state.list_cursor_spot().cloned(),
            detail: self.state.detail_spot(),
        }
    }

    /// Put the keyboard back at `to`, each part only where it still can be:
    /// the grid cursor on a card still on the board, the list's on a row the
    /// list still shows, and the detail's scroll only while that card is
    /// open. A cursor `to` doesn't have is left where it is.
    pub(crate) fn jump_to(&mut self, to: &HeadwayJump) {
        if let Some(card) = to.grid.filter(|&card| self.seen_cards.contains(card)) {
            self.state.set_cursor(card);
        }
        if let Some(spot) = &to.list {
            self.state.restore_list_cursor(spot);
        }
        if let Some(spot) = to
            .detail
            .filter(|spot| self.state.selected() == Some(spot.card))
        {
            self.state.scroll_detail(DetailScroll::To(spot.offset));
        }
    }

    /// [`jump_here`](Self::jump_here) as the chrome keeps it.
    pub(crate) fn jump_location_token(&self) -> JumpLocation {
        Rc::new(self.jump_here())
    }

    /// [`jump_to`](Self::jump_to) a location the chrome handed back; one
    /// that isn't Headway's is a no-op.
    pub(crate) fn restore_jump_token(&mut self, location: &dyn JumpTarget) {
        if let Some(to) = location.as_any().downcast_ref::<HeadwayJump>() {
            self.jump_to(to);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A jump back to a cursor on a card that has left the board leaves the
    /// cursor where it is.
    #[test]
    fn a_jump_to_a_departed_card_keeps_the_cursor() {
        let mut headway = Headway::new();
        let gone = NoteId::new([7; 32]);
        let here = NoteId::new([8; 32]);
        headway.state.set_cursor(here);
        headway.jump_to(&HeadwayJump {
            grid: Some(gone),
            list: None,
            detail: None,
        });
        assert_eq!(headway.state.cursor(), Some(here));
    }
}
