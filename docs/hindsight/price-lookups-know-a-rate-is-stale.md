# Price lookups know when a rate is stale

`ledgerdb::PriceDb` uses a pair's nearest earlier rate however old, on the
shortest path, so a series that ends prices its currency forever after.

- headway:cashflow/because-blue-clump — ~15 min: a hand-kept `ecb::FILLS` table of Valet's gaps (DKK after 2017, THB 2020–2026) so ECB crosses land on the same pair, and AED priced only through its USD peg because Valet's AED/CAD series ends in 2017 and would outrank the chain; RUB after 2026-04-30 still prices silently at April's rate
