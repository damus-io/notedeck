# A source can categorize another source's record

Derive categorizes a record only from edits, never from another source's record.
So payroll files a reclass beside `btc:<txid>`, which an override then doubles.

- headway:cashflow/slow-humble-goat — ~15 min: the card's `assets:btc` leg would have spent all 29 payouts twice; worked around by posting the payroll record's counter leg to `expenses:unknown` (a reclass), which leaves `btc:<txid>` looking uncategorized and turns an override of it into a double count
