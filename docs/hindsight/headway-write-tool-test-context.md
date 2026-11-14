# A headway tool test context that can write

`seeded_env` is watch-only with no publisher, so a write tool refuses to sign.
Testing who signs means booting `Notedeck --nsec` and polling a board to settle.

- headway:notedeck/strategy-humble-apart — built a `Notedeck::init --nsec` context plus a `settled_view` poll loop inside the unit tests to check who signs `headway_comment_card`
