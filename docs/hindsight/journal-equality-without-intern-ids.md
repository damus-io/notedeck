# Journal equality that ignores intern ids

Equal journals aren't `==`: their string, account and commodity ids follow
interning order, so each round-trip or dedup check writes a resolve helper.

- headway:cashflow/present-shuffle-cost — ~10 min: wrote a 100-line `resolve` in crates/ledgerdb/tests/write.rs to compare parse∘write∘parse against parse
- headway:cashflow/oyster-hard-submit — ~5 min: compared incremental vs full loads by canonical export text and wrote a qty_of-by-names helper in crates/ledgerdb/tests/ledger.rs, since Ledger ids follow load order
- headway:cashflow/balance-knee-abstract — ~10 min: wrote `Journal::same_txn` (crates/ledgerdb/src/novel.rs), a per-transaction compare by name and value; a whole-journal compare built on it would retire both test helpers above
