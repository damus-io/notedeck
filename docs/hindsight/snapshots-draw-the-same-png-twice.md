# A snapshot test draws the same png on every run

Some snapshot tests render differently between two runs of one commit, so the
check goes red with nothing changed and only a rerun tells flake from regression.

- headway:notedeck/balcony-what-club — ~10 min over three full release runs telling four flakes from stale pngs: horizon's two all-day events swap order (1 in 8), headway_list_hover_light's "Side quest card" folds into Todo or Backlog, command_bar_rich_rows differs by 27 px (50 in CI), session_outputs shifts its cards 2 px (2 in 22 after its spinner fix)
