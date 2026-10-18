# One correct rank helper for every ordered list

headway's `rank_between` lets ranks end in `a` (the 4th prepend sorts after the
3rd), so cashflow wrote its own `cashflow::rank` and the two now drift.

- headway:cashflow/wrestle-depart-cash — ~6 min: found the prepend hole while reusing `rank_between` for rule order, wrote `cashflow::rank` (between + spread) rather than patch headway's ranks mid-card
