# Books keep the raw ledger

`Books::load` drops the raw ledger that `transfers::suggest` and an "unknown"
view need, so a caller holding `Books` loads the store a second time.

- headway:cashflow/mention-example-nation — ~5 min: `transfers::suggest` takes a raw `Ledger` that `Books` doesn't expose, so its tests and its future dashboard caller load the store themselves
