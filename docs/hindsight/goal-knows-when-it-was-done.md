# A goal knows when its card was done

`goals::Goal` says its card is in a terminal column but not since when, so a
reader dating a goal's completion goes back to the folded board for `placed_at`.

- headway:cashflow/chef-win-fine — ~3 min: `adventure::quests::done_at(view, goal)` and `GoalBoards::view_of` in the CLI exist only to find a done goal's `placed_at` again, and every caller of the fold has to pair each goal with it in an `EpicGoal`
