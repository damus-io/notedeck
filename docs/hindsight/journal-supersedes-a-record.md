# The journal can supersede a record by id

A record leaves the journal only with its whole segment (`journal rm`), so an
importer that regroups old records can't withdraw them and must leave them be.

- headway:cashflow/cake-athlete-jewel — the cln importer had to leave every chain event the journal already held as its own record ungrouped (or count it twice), so history before the change keeps its unbalanced per-event halves; regrouping it means retiring every old `cln-chain` segment and importing a whole listing again
