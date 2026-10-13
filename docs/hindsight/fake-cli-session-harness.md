# A fake-CLI session harness for the Claude actor's tests

Each session-actor test in backend/claude.rs copies ~40 lines of fake-CLI,
channel and serve_session setup before its own script and asserts.

- headway:dave/general-tool-lecture — ~3 min: copied the setup a fifth time for stop_task_stops_a_background_task; a `FakeCliSession::spawn(script)` holding the command sender and response receiver would have left only the script and the asserts
- headway:dave/large-denial-card — the resume path (`task_started` keyed by the SendMessage call, text keyed by the original launch) was recorded live in the spike but tested as two halves, backend and session; a `FakeCliSession` would have replayed the spike's real stream end to end
