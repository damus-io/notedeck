# Seed a Dave with sessions and normal mode from outside its crate

Dave's sessions, overlay and normal mode are crate-private, so a chrome-level
test can host a real Dave but can't put it anywhere a key means something.

- headway:notedeck/juice-crystal-stumble — the full-chrome Dave key harness the card asked for couldn't be built (a fresh Dave has no sessions and test_dave is crate-private); chrome only checks the tool registry, and the key→tool→Dave tests run in-crate behind a copy of chrome's key step (ui_tools::chrome_keys)
