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
