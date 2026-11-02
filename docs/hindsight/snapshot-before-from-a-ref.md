# Render a snapshot test at another commit, for a before/after

A design card's explainer needs the same new snapshot test rendered on the old
code, and nothing renders a test against a base ref without hand-swapping files.

- headway:dave/group-angry-try — ~8 min: a new composer snapshot test had to render "before" on HEAD's sources; swapped dave.rs and git_status_ui.rs in from `git show HEAD:` twice (the first render raced the edit and caught half the new code), each a --release rebuild
- headway:headway/choose-plunge-once — ~5 min: rendered the new hover snapshots on HEAD's list.rs by swapping it in from `git show HEAD:` and back; a second swap, to check whether six other list pngs were already stale at HEAD, was refused as discarding work
- headway:dave/hunt-mixed-venue — ~5 min: to show the old chain at the same 2x scale as nine mock variants, re-implemented the old indent + rail as a throwaway `Before` mock style instead of rendering the test at the base ref
- headway:cashflow/elder-spirit-fly — ~6 min: proving the dashboard chart port pixel-identical took a WIP commit, `git checkout HEAD~1 -- crates/notedeck_dashboard/src`, an UPDATE_SNAPSHOTS render copied to scratch, then a restore and a compare run against it
- headway:notedeck/song-suggest-decorate — ~28 of a 38-min session: proving debug and release pngs bit-identical took two force-mode sweeps over all nine suites, the first sweep's pngs copied to scratch and restored by hand, and a throwaway Pillow script to diff 181 pairs
- headway:notedeck/ball-lawn-zoo — ~10 min: headway_review_explainer_figures had no png, so "figures look unchanged" needed a before: WIP commit, `git checkout HEAD~1 -- crates/`, an --update render, restore, then a Pillow shift-compare via nix-shell; repeated after a rebase onto a concurrent layout change
- headway:headway/twelve-circle-smart — ~3 min: swapped review_figures.rs and review_sources.rs in from `git show HEAD:` to render the new ba snapshot as a before, and it timed out, since the test waits on a caption only the new code draws as a label; used the card's old screenshot instead
- headway:headway/tumble-wet-topple — 40 min session, ~20 of it here: `--changed --debug` ran every suite under notedeck core (~15 min), then six unrelated png failures meant `git checkout <base> -- crates assets`, rerunning two suites, and restoring to show that notebook_note_embed fails on base too and cashflow_detail_rule_preview only flakes
