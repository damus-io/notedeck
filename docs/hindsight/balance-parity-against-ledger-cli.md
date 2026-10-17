# A balance parity check against ledger-cli

No `ledger` or `hledger` in the dev shell, so nothing checks `report bal` against
ledger-cli; a real-data run hand-writes an independent summer to trust its numbers.

- headway:cashflow/travel-size-carpet — ~4 min writing a throwaway Python per-root, per-commodity summer (with elided-amount inference) to compare against `report bal --native --depth 1` on the legacy files
