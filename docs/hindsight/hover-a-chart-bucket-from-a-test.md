# Hover a time-series chart's bucket from an app-level test

An app test sees only a chart's widget rect, not its plot rect, which starts
past y-axis labels of unknown width, so it guesses where bucket `i` sits.

- headway:cashflow/clinic-autumn-fault — ~4 min: a span test hovered the wrong month (labels narrower than guessed); `hover_month` in tests/trends.rs now counts back from the right edge with a 40pt label guess, which is only exact near the right
