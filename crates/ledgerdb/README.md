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
