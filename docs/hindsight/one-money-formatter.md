# One formatter for an i128 sum of a commodity

ledgerdb's `Amount` holds an `i64`, but every query sums into `i128`, so each
reader writes its own "fits an i64? display it : print raw units" wrapper.

- headway:cashflow/response-absent-royal — ~2 min: copied cashflow_cli's `format_total` (books_cmd.rs) into notedeck_cashflow's categories model as `money`, and a signed `delta` beside it
- headway:cashflow/clinic-autumn-fault — ~2 min: moved Categories' `money` into `notedeck_cashflow::money` so Trends could share it, and wrote a compact axis formatter beside it; cashflow_cli's `format_total` is still a separate copy
- headway:cashflow/group-matter-salad — ~2 min: wrote a second signed formatter, `overview::model::delta` (`+619.89 CAD`), beside Categories' `delta_text`, because `money` has no signed form
