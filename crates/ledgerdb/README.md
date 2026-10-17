# ledgerdb

A generic, fast, in-memory ledger engine for journals written in a subset of
the [ledger-cli](https://ledger-cli.org) format.

ledgerdb knows nothing about where its data comes from. It has no knowledge
of banks, wallets, card issuers or any particular person's books, and it has
no dependency on notedeck, egui or nostrdb. Source-specific importers and any
UI live in the crates that use it. It lives in the notedeck workspace for now
and is meant to move to its own repository unchanged.

## Design

- **Plain old data.** A posting is a 24-byte `Copy` struct (`Posting`) with
  no heap data: a date, interned account and commodity ids, flags, the index
  of its transaction, and an integer quantity. Postings live in contiguous
  arrays, so a balance or a flow is a linear scan over memory.
- **Integers, not floats.** An `Amount` is an `i64` in its commodity's
  smallest unit (cents, millisatoshis, ...). Each commodity has a decimal
  precision, and parsing an amount finer than that precision is an error,
  never a silent rounding.
- **Interned everything.** Accounts are interned colon paths with parent
  links (`expenses:food:coffee` interns `expenses` and `expenses:food`), and
  every other string (payees, codes, metadata, record ids) lives in one
  `StrPool` arena addressed by `StrId`.
- **Built in memory.** There is no database and no on-disk snapshot; the
  ledger is rebuilt from its journal at startup.
- **Reading never aborts.** `Journal::parse` reads the ledger-cli subset in
  two zero-copy passes (the first learns every commodity's precision, so no
  amount is converted before it is known) and returns the journal together
  with every `ParseError`, each with its line and column. A transaction with
  any error, including one that does not balance, is left out whole.
- **One canonical text form.** `Journal::write_ledger` (and `to_ledger`,
  `write_txns` for a subset) writes the journal back as ledger-cli text that
  parses to the same journal. The layout is fixed, not configurable: tab
  indentation, a tab between account and amount, every comment on its own
  line, one blank line between sections, amounts in each commodity's learned
  style. Identical journal contents always produce identical bytes, which is
  what lets journal segments be named by the hash of their text.

## Journal segments

A journal on disk is a directory of **segments**, one per import batch,
managed by `JournalStore`. A segment is a four-line header (`; ledgerdb-segment:
1`, `; source:`, `; imported:` unix seconds, `; layer:`), a blank line, and
the batch in the canonical form above, every transaction carrying an `; id:`.
It is filed at `<dir>/<first two hex>/<sha256 of its bytes>.ledger`.

**The layout is write-once and named by hash.** A segment is written to a
temporary file beside its final name, synced and renamed into place, and
never changed afterwards; every read checks the bytes against the name, so a
corrupt or hand-edited file is an error naming it. Because a name always
means the same bytes, any number of copies of a journal union without
conflicts: copying files in any direction, by rsync, git or anything else,
can only add segments, never clash. Files that are not hash-named (temporary
files, rsync partials, `.git`) are ignored.

Anything fetched from another machine comes in through
`JournalStore::admit_from(staging)`, which checks each staged file's hash and
header before writing it the same way a local import is written, and reports
the files it refused without touching them. A transport fetches into staging
and never writes into the journal directory itself.

## The in-memory ledger

`Ledger::load(&store, filter)` reads every segment and merges the ones whose
layer the `LayerFilter` includes (`All`, `Only([...])`, `Except([...])`, the
equivalent of choosing which files to pass `ledger -f`) into one `Journal`
with shared account, commodity and string tables. Nothing is written back:
the ledger is rebuilt from the store at startup, off the UI thread.

- **Sorted arrays.** Transactions are sorted by date, then segment hash,
  then position in the segment, and postings follow them, so the posting
  array is sorted by `(date, txn)`. A per-account index lists each account's
  postings in that order. The order depends only on which segments are
  loaded, not on the order they arrived in.
- **One transaction per record id.** When two segments carry the same
  `; id:`, the later `imported` header wins (then the larger segment hash,
  then the later position), and the loser is left out. A re-export that
  turns a pending row into a posted one supersedes it without losing the
  edits keyed on its id.
- **Exact precision.** A commodity takes the finest precision any loaded
  segment wrote it with, and coarser quantities are multiplied up to it; a
  rescale that would overflow refuses the segment and leaves the ledger as
  it was.
- **Incremental.** `Ledger::refresh(&store)` merges only the segments the
  ledger has not seen, loaded or excluded, and `Ledger::add_segment` merges
  one already read. Either re-sorts the arrays in memory without reading
  other segments again, and the result equals a full load of the same
  segments.
- **Never aborts.** A segment that cannot be read or merged is reported in
  the `LoadReport` and tried again on the next refresh; a segment with
  transactions that did not parse loads without them and is reported as
  damaged.

The `Ledger` exposes the arrays read-only; `ledger.journal()` is the merged
journal, so its writer exports the whole ledger as ledger-cli text.

## Prices and conversion

`PriceDb::from_ledger(&ledger)` turns the ledger's `P` directives into a
graph of commodities, each edge holding one pair's rates in date order, and
converts an amount at the date it happened:

- **No future prices.** Each hop uses its most recent rate on or before the
  date, so a 2019 posting is never valued at a 2024 rate.
- **Chained.** `rate(from, to, date)` takes the fewest-hop path
  (`msat → sat → BTC → USD → CAD`), found by breadth-first search over the
  edges that exist by the date and memoized per pair. Every price also
  prices the other way, as its inverse.
- **Exact.** A `Rate` is a reduced `i128` fraction and a chain multiplies
  fractions. `convert(amount, to, date)` rounds once, to the nearest unit of
  the target's precision with halves away from zero.
- **Never guessed.** A missing rate is `None`; `convert_counting` tallies
  misses per pair with their date range in an `Unpriced`, so a view can mark
  what it could not price instead of showing a wrong total.

On a date with several prices for one pair the one from the segment with
the largest hash wins, so the result does not depend on load order.
