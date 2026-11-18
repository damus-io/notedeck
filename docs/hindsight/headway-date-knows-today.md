# headway's Date knows today and its unix second

`headway::event::Date` has no local today and no midnight in unix seconds, so
each caller comparing due dates converts through chrono by hand.

- headway:dave/bar-inject-train — ~1 min: `today()` and `log_since()` in `notedeck_dave::review_schedule` map chrono's `NaiveDate` to and from `Date` field by field to compare due dates and bound a nostrdb `since`
