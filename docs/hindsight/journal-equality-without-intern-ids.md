# Journal equality that ignores intern ids

Equal journals aren't `==`: their string, account and commodity ids follow
interning order, so each round-trip or dedup check writes a resolve helper.

- headway:cashflow/present-shuffle-cost — ~10 min: wrote a 100-line `resolve` in crates/ledgerdb/tests/write.rs to compare parse∘write∘parse against parse
