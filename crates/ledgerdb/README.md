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

## Deriving the categorized ledger

Importers write raw facts: the leg they are sure of, plus a **counter leg**
to a structural default such as `expenses:unknown`, with what they read as
`; raw-*` metadata. `derive(&raw, &edits)` moves each counter leg to its
real account and returns a `Derived`: the categorized `Ledger`, which
transaction decided what, and a report of the edits it could not apply.
The edits are plain input data; storing and syncing them is the caller's
business.

- **The counter leg** is a transaction's one posting to an account in
  `Edits::defaults`. A transaction with none, or several, passes through
  untouched, so hand-entered, fully categorized ones are left alone.
- **Priority.** A `Split` or `Override` of the transaction's record id,
  then a `TransferLink`, then the first `Rule` whose conditions all hold,
  then the importer's default. A rule's conditions are a case-insensitive
  substring of the payee or a metadata value, an exact payee or metadata
  value, the counter leg's magnitude in an inclusive range of smallest
  units, and the segment's source; a rule can also be limited to a layer.
- **First wins, nothing silently.** Rules are tried in order. Two edits
  claiming one transaction resolve the same way (a split beats an
  override, otherwise the earlier one wins) and the loser is reported. A
  split whose legs do not sum to the counter leg, a link whose two legs do
  not net to zero, or an invalid account name is rejected whole and the
  transaction falls through to the next tier. A link takes both its
  transactions or neither, so a transfer never half-nets.
- **Same positions.** The derived ledger has the raw one's transactions at
  the same `TxnId`s; `Derived::explain(record_id)` names the override,
  split, link or rule that decided one, for a tooltip.
- **Deterministic.** Edits are keyed by account names and record-id
  strings, never interned ids, so every machine with the same segments and
  edits derives byte-identical exports.
- **Fast enough to re-run.** `Contains` needles are found with one
  Aho-Corasick pass per field, and rules are indexed by their first
  `Contains` condition, so a transaction only tries the rules whose needle
  it contains. A full derive of 350k transactions (700k postings) with 200
  rules and 1000 overrides takes about 100 ms in release, so a changed
  edit re-derives everything rather than tracking what it touched.

## Querying

`Rollups::new(&ledger, &prices, Some(home))` indexes a ledger, usually the
derived one, once per load or derive: the account tree laid out so each
subtree is one contiguous run, every posting converted to the home
commodity at its own date, and those summed per account, month and
commodity. `rollups.query(&ledger, &prices)` pairs it with the ledger and
answers:

- `balance(account, at, in)`: the subtree's balance at the end of a day,
  natively per commodity or as one converted total.
- `flows(account, range, bucket, in)` / `flows_into(.., &mut Series)`: the
  subtree's per-day, -week (Monday), -month or -year sums over a range, for
  a cashflow-over-time chart.
- `top_n(parent, depth, range, n, in)`: the largest accounts at a tree
  depth, each covering its subtree, with their shares of the total.
- `register(Filter { .. })`: a lazy iterator over the transactions matching
  an account subtree, a date range, a payee substring, a metadata key, a
  record id and "posts to one of these (default) accounts", all optional.

Whole months in the home commodity come from the cells; partial months at
a range's ends, and day or week buckets, read the postings in range. The
answers are exact either way, to the unit, because a cell is the sum of
its postings each converted and rounded at its own date. A posting with
no rate at its date is left out of the converted total and reported in an
`Unconverted`, natively. Converting to anything other than the home
commodity converts as it scans, and is several times slower.

Over 350k transactions (700k msat postings, 4300 accounts, a daily rate)
building the index takes about 90 ms, and balances, monthly flows over
all time and top-N over 4000 accounts each take a few milliseconds or
less in release (`tests/bench_query.rs`). Rebuilding after every change
keeps it simple; the index is checked against the ledger it is queried
with and panics on a mismatch rather than answering wrongly.
