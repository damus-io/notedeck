# Books keep the raw ledger

`Books::load` drops the raw ledger that `transfers::suggest` and an "unknown"
view need, so a caller holding `Books` loads the store a second time.

- headway:cashflow/mention-example-nation — ~5 min: `transfers::suggest` takes a raw `Ledger` that `Books` doesn't expose, so its tests and its future dashboard caller load the store themselves
- headway:cashflow/wrestle-depart-cash — the rule commands' dry runs derive twice from a raw `Ledger` they load themselves (`rule_cmd::raw_ledger`), beside the `Books` export loads
- headway:cashflow/name-draw-animal — `categorize`/`split`/`transfer` derive twice from `books_cmd::raw_ledger` for their before/after, and `transfer suggest` loads it again beside the `Books` `unknown` builds
