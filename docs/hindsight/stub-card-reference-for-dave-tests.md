# One stub `headway:` card parser and renderer for Dave's tests

Each Dave test needing a card chip writes its own stand-in parser, and no
session-list snapshot draws with `title_refs`, so a lost row chip goes unseen.

- headway:dave/celery-wink-else — the chip a card-titled session row lost was in no snapshot; copied reference_chip_tests' StubCardParser/StubCardRenderer into session_list's unit tests (FocusQueue and CollapseState are crate-private) and added `title()` so the card-title lookup finds anything
