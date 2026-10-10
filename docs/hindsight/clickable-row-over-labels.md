# A whole-row click target that sits above the row's labels

Cards make a row clickable by calling `interact` on its layout's response, which
keeps the layout's earlier hit-test slot under its own labels, so clicks miss.

- headway:dave/matrix-message-outdoor — ~4 min: the Files card's edited-file rows took no clicks (contains_pointer true, hovered false) until a debug print showed it; re-registered the rect under a fresh `ui.interact` id, where Subagents' Frame-based row had worked by accident
- headway:dave/fringe-cup-pigeon — ~10 min: a session-list title chip took the row's click from three stacked senses (open_on_click's interact, card_chip_ui's own click sense, the chip's selectable Label); ignoring the chip's action alone wouldn't have let the click through
