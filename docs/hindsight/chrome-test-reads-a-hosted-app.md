# A chrome test reads and clicks the app it hosts

Once a seeded app is boxed into `Chrome`, a test sees only painted text and
`active_app()`: it can't read the app's state or click one of its rows.

- headway:notedeck/wire-snack-open — ported the `E` macros onto a real Dave but not G-then-Ctrl-O (Dave's block cursor can't be read through the chrome) or subagent-back (the subagent row opens only on a click, and ChromeDriver can't click by label); both stay in-crate behind `ui_tools::chrome_keys` and nav.rs
- headway:ux-feedback/filter-rail-dwarf — ~5 min: ChromeDriver has no AccessKit queries, so the tab-x click test built its own kittest state (ui_tests `TabStripState`) owning the Chrome beside `chrome_frame`, to find the tab label and its x by label and still read `is_opened` afterwards
