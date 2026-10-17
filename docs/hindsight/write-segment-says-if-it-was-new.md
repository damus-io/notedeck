# JournalStore::write_segment says whether the segment was new

`write_segment` returns only the hash, so a caller that reports "already in
the journal" encodes the batch a second time just to hash it first.

- headway:cashflow/afford-nuclear-grass — ~3 min, and a double encode per import: `cashflow::import::write` runs `Segment::encode` to learn the hash for `store.contains`, then `write_segment` encodes again; ledgerdb's private `Put::{Written, Present}` already knows
