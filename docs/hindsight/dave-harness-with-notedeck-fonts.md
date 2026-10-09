# A Dave kittest harness set up like the chrome: notedeck fonts and app spacing

Dave snapshots build bare harnesses on egui defaults, so layout and baseline bugs
that only show with notedeck's fonts and spacing never reach a test.

- headway:headway/skill-either-bench — glued tool rows were invisible to every Dave snapshot; wired one test through a hand-made mirror of the chrome (x = 0 + setup_fonts + own_item_spacing)
- headway:headway/endorse-language-brave — no committed test renders Dave with notedeck's fonts (diff_view_tests, markdown_table_tests use bare build_ui), so the Dave half of the mono baseline regression was invisible to CI
- headway:dave/okay-scale-wrap — the session-view kittest skipped the chrome's root panel fill, so the first Details snapshot drew dark-theme cards on a white clear and read as a theme bug
