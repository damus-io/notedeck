# ledgerdb::Date does calendar arithmetic

`Date` has `first_of_month` but no days-after, weekday, week start or month end,
so each caller writes `Date::from_days(d.days() + n)` and its own Monday maths.

- headway:cashflow/chef-win-fine — ~4 min: `plus` and `monday_of` in `cashflow::adventure::quests` (the epoch is a Thursday, so a week starting before it saturates; the first cut stepped Thursday to Thursday from there), and a month's last day by hand in `achievements::History::met_on`
