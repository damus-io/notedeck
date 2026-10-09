# One helper for an account's private latest-wins note, read and written

Each app hand-rolls the same PNS replaceable-note pair: read the newest
revision of a kind + d, ties broken by id, and write one stamped past it.

- headway:notedeck/tourist-wire-joy — chrome's keymap note (keymap/user.rs current/write_user_keymap) re-derives headway's load_board_pref + save_board_pref/next_after, and headway's read still has no id tie-break
