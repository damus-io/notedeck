# agentium-core tests ingest a batch of events through one helper

`test_support` has no ingest-and-wait for `BuiltEvent`s, so each test module
writes its own subscribe, `process_event_with`, `wait_for_notes` loop.

- headway:notedeck/flower-foam-attitude — cli/grep.rs tests carry a third copy of `ingest` (beside split_message.rs's and session_loader.rs's `ingest_all`)
