# Snapshots render on a Mac without Docker

`scripts/snapshot-test` needs Docker for lavapipe, and the harness refuses a GPU
adapter, so a session without Docker can't even look at a row it restyled.

- headway:headway/end-kick-draft — the list row was rebuilt to Linear's anatomy and nobody has seen it: Docker Desktop wouldn't start in two minutes and `snapshot_headway_list` panics "No CPU adapter found" natively; a Metal preview mode (render, don't compare) would have let the session eyeball it against linear-3
- headway:headway/match-belt-quit — `headway_list.png` was never generated for the same reason
- headway:headway/album-trouble-treat — `inline_chip.png` left stale for the same reason
