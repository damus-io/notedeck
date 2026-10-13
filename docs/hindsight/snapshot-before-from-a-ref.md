# Render a snapshot test at another commit, for a before/after

A design card's explainer needs the same new snapshot test rendered on the old
code, and nothing renders a test against a base ref without hand-swapping files.

- headway:dave/group-angry-try — ~8 min: a new composer snapshot test had to render "before" on HEAD's sources; swapped dave.rs and git_status_ui.rs in from `git show HEAD:` twice (the first render raced the edit and caught half the new code), each a --release rebuild
- headway:headway/choose-plunge-once — ~5 min: rendered the new hover snapshots on HEAD's list.rs by swapping it in from `git show HEAD:` and back; a second swap, to check whether six other list pngs were already stale at HEAD, was refused as discarding work
