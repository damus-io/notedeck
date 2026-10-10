# A commit that changes a snapshot's render fails until it re-records the png

Card: headway:notedeck/balcony-what-club
Snapshot tests skip off lavapipe, so a commit can change a png's render unrecorded;
the next card to run `--update` has to sort those pngs from its own.

- headway:dave/february-album-decrease — re-recording the session-list pngs also rewrote dave_input_normal_mode_cue and settings_session_env{,_narrow}, stale since 48f600b9c99b retired the leader key; spent ~5 min diffing them to prove they weren't mine and reverting them
- headway:dave/tone-grief-hour — re-recording the session-list pngs also rewrote eight others (block_cursor_rows, session_background, session_outputs, session_rate_limits, tool_rows_under_chrome_spacing, dave_input_normal_mode_cue, settings_session_env{,_narrow}); reverted them unexamined
- headway:dave/throw-draw-draw — re-recording the session-list pngs also rewrote the same eight unrelated ones (block_cursor_rows, session_files, session_outputs, …); reverted them unexamined
- headway:notedeck/next-various-calm — headway_switcher_same_slug fails at HEAD (the switcher menu narrower, "Delete board…" wraps); ~3 min proving with a WIP commit and a rerun that it predates the card, left unrecorded
- headway:headway/choose-plunge-once — re-recording the list pngs also rewrote six others whose status groups 3d4278b7b512/48704218a163 reordered unrecorded; ~5 min with magick compare to prove the diff was group order only, then committed them separately; snapshot_headway_list_key_hints still fails from the same reorder
- headway:dave/hunt-mixed-venue — re-recording the two lineage pngs also rewrote ~12 others (block_cursor_rows, executed_tool_results{,_expanded,_hover}, session_files, session_outputs, settings_session_env{,_narrow}, websearch_result, …) and created clamped_bash_output{,_show_all}; reverted them unexamined
- headway:dave/diagram-track-gym — re-recording all dave snapshots for one lineage png also rewrote the same ~12 (block_cursor_rows, executed_tool_results*, settings_session_env*, websearch_result, …) and created clamped_bash_output{,_show_all}; reverted them unexamined
- headway:dave/shallow-upgrade-pet — the full dave suite still fails the same 12 stale pngs (tool rows, settings_session_env, session_outputs, clamped_bash_output…); re-recorded only chat_times_* and the new header_walltime_* by filter
- headway:notedeck/when-exercise-arch — 15 dave pngs failed after a test-harness port; ~15 min on a --release baseline of the base commit in a scratch worktree to split them: 12 stale before the card (the set above), 3 mine (a spinner one frame further on)
