# Headway settles the view it will draw before keys, tools and overlays read it

Card: headway:notedeck/snake-unusual-coast
which view a frame draws (settled, outgoing, after its drops) is known only
while drawing, so every per-view transient guesses it from reseeded state.

- headway:headway/tree-produce-ivory — the notice, X composer and pane_chord each guess whether they are the settled top, the outgoing top or beneath; wanted a NavPass on AppContext
- headway:headway/skin-labor-provide — nav_pos() is read before board_pane_ui applies its drops; a settle_view at the top of board_ui would have made the card a no-op
- headway:notedeck/agree-liar-auto — ui_tool_available has no BoardView outside render, so availability needed a per-pass session latch
- headway:notedeck/next-various-calm — `]b`/`[b`'s availability needs the switcher's board list, which only render builds, so it took another per-pass latch (`BoardUiState::other_boards`)
- headway:notedeck/ice-basket-taxi — the issue list's `c` is offered only where the cursor's group takes a card on the board showing (`group_add` needs the `BoardView`), so it took a third per-pass latch (`ListState::can_compose`)
