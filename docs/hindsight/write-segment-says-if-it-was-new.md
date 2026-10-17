# JournalStore::write_segment says whether the segment was new

`write_segment` returns only the hash, so a caller that reports "already in
the journal" encodes the batch a second time just to hash it first.

- headway:cashflow/afford-nuclear-grass — ~3 min, and a double encode per import: `cashflow::import::write` runs `Segment::encode` to learn the hash for `store.contains`, then `write_segment` encodes again; ledgerdb's private `Put::{Written, Present}` already knows
- headway:cashflow/travel-size-carpet — no minutes, but the first real run pays it on a 46 MB segment: importing cln.ledger encodes 334k transactions twice (1.3 s for the whole import)
- headway:cashflow/truth-travel-begin — ~2 min, and a third copy of the workaround: `cashflow::prices::fetch_into` encodes each year segment for `store.contains` and then `write_segment` encodes it again, same as `import::write`
