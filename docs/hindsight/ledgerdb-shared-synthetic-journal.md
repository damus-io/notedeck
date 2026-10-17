# One synthetic journal generator for ledgerdb tests and benches

tests/bench_parse.rs holds the CLN-shaped `synthetic()` journal, so each bench
copies it; a shared tests/support module would keep one shape for all numbers.

- headway:cashflow/oyster-hard-submit — ~5 min: copied Rng + synthetic() into tests/bench_ledger.rs and adapted it for ids and segment ranges
- headway:cashflow/glue-saddle-forest — ~3 min: copied parse_ok and the encode→parse segment helper from tests/ledger.rs into tests/price.rs
- headway:cashflow/boost-wish-jar — ~3 min: copied Rng into tests/bench_derive.rs and parse_ok + the encode→parse segment helper into tests/derive.rs
