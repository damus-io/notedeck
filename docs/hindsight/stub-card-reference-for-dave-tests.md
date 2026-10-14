# One stub `headway:` card parser and renderer for Dave's tests

Each Dave test needing a card chip writes its own stand-in parser, and no
session-list snapshot draws with `title_refs`, so a lost row chip goes unseen.

- headway:dave/celery-wink-else — the chip a card-titled session row lost was in no snapshot; copied reference_chip_tests' StubCardParser/StubCardRenderer into session_list's unit tests (FocusQueue and CollapseState are crate-private) and added `title()` so the card-title lookup finds anything
- headway:notedeck/field-fever-follow — the Cmd-K search test needed the same stubs; moved session_list's into a cfg(test) `crate::test_refs` (plus `ingest_card`), but tests/reference_chip_tests.rs is an integration test, can't reach it, and keeps its own copy
