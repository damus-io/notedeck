# ledgerdb::Date converts to and from unix seconds

`Date` counts days since 1970 but has no `from_unix_secs`/`unix_secs`, so each
caller divides by 86 400 itself and keeps its own `now()`.

- headway:cashflow/truth-travel-begin — ~3 min: a second `day_of(secs)` in `cashflow::prices` beside `cashflow_cli::journal_cmd`'s, a second `SystemTime` now in `prices_cmd` beside `import::now`, and `days() * 86_400` by hand for each segment's `imported` stamp
- headway:cashflow/pyramid-fluid-bomb — ~2 min: a third `secs / 86_400` in `cashflow::import::btc::date_of` for a block's time
- headway:cashflow/brick-hospital-race — ~1 min: a fifth `secs / 86_400`, `cashflow::import::appstore::today` for the default months
- headway:cashflow/oblige-regular-over — ~2 min: a sixth `/ 86_400`, `cashflow::import::cln::Zone::date`, which adds the time zone's offset first; a `Date::from_unix_secs(secs, offset)` would have been the whole function
- headway:cashflow/junk-brave-fox — ~1 min: moving `Zone::date` into `cashflow::zone` for the synced setting kept its `div_euclid(86_400)` and clamp; with a `Date::from_unix_secs(secs, offset)` the three zone arms would each be one call
- headway:cashflow/boat-burst-capital — ~1 min: a seventh `imported / 86_400` in `notedeck_cashflow::views::settings::model` for a source's last import, and `days() * 86_400` again in its test fixture
