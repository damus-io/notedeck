# One helper for an account's private latest-wins note, read and written

Each app hand-rolls the same PNS replaceable-note pair: read the newest
revision of a kind + d, ties broken by id, and write one stamped past it.

- headway:notedeck/tourist-wire-joy — chrome's keymap note (keymap/user.rs current/write_user_keymap) re-derives headway's load_board_pref + save_board_pref/next_after, and headway's read still has no id tie-break
- headway:dave/spirit-tower-opera — the kind-31990 inspector snapshot is one more private latest-wins note: latest_snapshot re-rolls the kinds+authors+#d limit-1 read and SnapshotPublisher re-rolls next_state_created_at stamping
