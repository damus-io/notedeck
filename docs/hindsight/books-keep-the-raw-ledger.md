# Books keep the raw ledger

`Books::load` derives and drops the raw ledger, but anything that reasons
about the importers' own counter legs (`cashflow::transfers::suggest`, a
"why is this unknown" view) needs the raw one, so a caller holding `Books`
has to load the store a second time or thread the raw ledger alongside.

- headway:cashflow/mention-example-nation — ~5 min: `transfers::suggest` takes a raw `Ledger` that `Books` doesn't expose, so its tests and its future dashboard caller load the store themselves
