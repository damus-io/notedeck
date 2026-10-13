# A commit that changes a snapshot's render fails until it re-records the png

Snapshot tests skip off lavapipe, so a commit can change a png's render unrecorded;
the next card to run `--update` has to sort those pngs from its own.

- headway:dave/february-album-decrease — re-recording the session-list pngs also rewrote dave_input_normal_mode_cue and settings_session_env{,_narrow}, stale since 48f600b9c99b retired the leader key; spent ~5 min diffing them to prove they weren't mine and reverting them
- headway:dave/tone-grief-hour — re-recording the session-list pngs also rewrote eight others (block_cursor_rows, session_background, session_outputs, session_rate_limits, tool_rows_under_chrome_spacing, dave_input_normal_mode_cue, settings_session_env{,_narrow}); reverted them unexamined
- headway:dave/throw-draw-draw — re-recording the session-list pngs also rewrote the same eight unrelated ones (block_cursor_rows, session_files, session_outputs, …); reverted them unexamined
