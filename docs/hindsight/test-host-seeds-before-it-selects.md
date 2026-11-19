# A test host seeded before its account reads the seed

`TestHost::with_account` selects before a test ingests, so contacts stay unreceived;
a button test that reads them selects a throwaway account, then its own again.

- headway:notedeck/wait-marine-boss — `nostr_tools/tests.rs` `press_follow` adds and selects a throwaway account, then alice again, so `ContactState` reads the seeded kind 3 before `send_kind_3_event` runs; a `TestHost::with_account_and(notes)` that ingests before it selects would have skipped it
