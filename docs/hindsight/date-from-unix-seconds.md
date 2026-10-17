# ledgerdb::Date converts to and from unix seconds

`Date` counts days since 1970 but has no `from_unix_secs`/`unix_secs`, so each
caller divides by 86 400 itself and keeps its own `now()`.

- headway:cashflow/truth-travel-begin — ~3 min: a second `day_of(secs)` in `cashflow::prices` beside `cashflow_cli::journal_cmd`'s, a second `SystemTime` now in `prices_cmd` beside `import::now`, and `days() * 86_400` by hand for each segment's `imported` stamp
