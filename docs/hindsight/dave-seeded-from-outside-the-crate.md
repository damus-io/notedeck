# Seed a Dave with sessions and normal mode from outside its crate

Dave's sessions, overlay and normal mode are crate-private, so a chrome-level
test can host a real Dave but can't put it anywhere a key means something.

- headway:notedeck/juice-crystal-stumble — the full-chrome Dave key harness the card asked for couldn't be built (a fresh Dave has no sessions and test_dave is crate-private); chrome only checks the tool registry, and the key→tool→Dave tests run in-crate behind a copy of chrome's key step (ui_tools::chrome_keys)
- headway:notedeck/write-stock-illness — the `E` macro's end-to-end test drives the real Headway but a stand-in Dave (an `App` advertising `dave_open_issue` in `dave.normal`), since a real Dave can't be given a session with an issue from chrome's tests
- headway:dave/large-success-wet — the card's "G then Ctrl-O through a harness" test for Dave couldn't run in chrome (no way to give a hosted Dave a session with blocks); Dave's hooks are tested in-crate and the keys through chrome on a FakeApp, so nothing checks the two meet
- headway:dave/slender-armed-spice — the card's "open a subagent, chrome back lands on the parent" test couldn't run through the chrome (no hosted Dave with a subagent); nav.rs tests drive seed_from_route/leave_route, so nothing checks that the chrome's pop reaches Dave's cleanup_nav
