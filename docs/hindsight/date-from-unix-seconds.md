# ledgerdb::Date converts to and from unix seconds

`Date` counts days since 1970 but has no `from_unix_secs`/`unix_secs`, so each
caller divides by 86 400 itself and keeps its own `now()`.

- headway:cashflow/truth-travel-begin — ~3 min: a second `day_of(secs)` in `cashflow::prices` beside `cashflow_cli::journal_cmd`'s, a second `SystemTime` now in `prices_cmd` beside `import::now`, and `days() * 86_400` by hand for each segment's `imported` stamp
- headway:cashflow/pyramid-fluid-bomb — ~2 min: a third `secs / 86_400` in `cashflow::import::btc::date_of` for a block's time
- headway:cashflow/brick-hospital-race — ~1 min: a fifth `secs / 86_400`, `cashflow::import::appstore::today` for the default months
