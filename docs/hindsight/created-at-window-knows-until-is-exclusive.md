# A created_at window that knows nostrdb's `until` is exclusive

Windowed reconciles split [since, until] inclusively; nostrdb matches created_at <
until, so each split's boundary event is silently skipped, never reported.

- headway:notedeck/purse-balance-mean — 6 min: a progress test came up 2 of 45 events short, and both missing events sat on a split's `until`
