# ledgerdb names a transaction's counter leg

Derive finds a transaction's counter leg with a private `counter_leg`, so each
caller that shows, splits or groups by it re-finds it by `edits.defaults` name.

- headway:cashflow/name-draw-animal — ~10 min: `edit_cmd::counter_leg` (split amounts), `Categorized::of` (which legs an edit moved) and `unknown::groups` each re-derive it, beside `transfers::suggest`'s own copy
