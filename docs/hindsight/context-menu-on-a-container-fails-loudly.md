# context_menu on a container fails loudly

egui ≥0.32 never opens a `context_menu` on a `scope` or `Frame` response.
Nothing flags it, so Copy menus died silently in the 0.36 upgrade.

- headway:dave/student-local-solution — the chat's Copy menus had been dead since b94fa6d8e280 with no test or lint noticing; a debug assert in `notedeck_ui::context_menu::context_menu` that its response senses clicks would have caught it the day of the upgrade
