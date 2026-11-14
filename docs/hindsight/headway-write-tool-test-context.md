# A headway tool test context that can write

`seeded_env` is watch-only with no publisher, so a write tool refuses to sign.
Testing who signs means booting `Notedeck --nsec` and polling a board to settle.

- headway:notedeck/strategy-humble-apart — built a `Notedeck::init --nsec` context plus a `settled_view` poll loop inside the unit tests to check who signs `headway_comment_card`
- headway:notedeck/bundle-hood-stick — built a second `Live` harness (`tools/tests/edits.rs`: `Notedeck::init --nsec`, seeded demo board, `card_until` poll) to port the CLI's edit e2e cases, since the first one lives inline in one test
- headway:notedeck/coyote-color-quantum — moved `Live` out of `edits.rs` into `tools/tests/live.rs` so the structure tests could share it; still a full `Notedeck::init` and a poll per read
- headway:notedeck/plug-dry-virus — `live()` settles on the demo board's 7 cards, not its last event, so the migrate test (which reads every note) flaked 1 in 3; it seeds its own board with `await_notes`, which `live()` can't use since the async tests can't nest its runtime
- headway:notedeck/brief-bar-number — wrote a third `Live` (`notedeck_notebook/src/tools/tests.rs`: `Notedeck::init --nsec`, `until` poll over `notebook_show`) for the notebook tools, since headway's lives in headway's private test module; one `notedeck_testing` live tool context would serve every app
