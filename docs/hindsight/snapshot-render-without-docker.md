# Snapshots render on a Mac without Docker

Card: headway:notedeck/rebuild-coach-blade
`scripts/snapshot-test` needs Docker for lavapipe, and the harness refuses a GPU
adapter, so a session without Docker can't even look at a row it restyled.

- headway:headway/end-kick-draft — the list row was rebuilt to Linear's anatomy and nobody has seen it: Docker Desktop wouldn't start in two minutes and `snapshot_headway_list` panics "No CPU adapter found" natively; a Metal preview mode (render, don't compare) would have let the session eyeball it against linear-3
- headway:headway/match-belt-quit — `headway_list.png` was never generated for the same reason
- headway:headway/album-trouble-treat — `inline_chip.png` left stale for the same reason
- headway:headway/office-flower-churn — the group headers (fold arrow, count, `+`) and the composer bar have only been checked through accesskit; `snapshot_headway_list` now draws them grouped by status and still has no PNG
- headway:headway/battle-stock-expire — the work-order ready bar and the drag insertion line are painter-only, so neither accesskit nor any PNG has ever shown them; the behavioural test proves the drop writes the order, not that a person can see where it will land
- headway:dave/beef-calm-cross — Docker Desktop wouldn't start from the session and rendering on the NixOS host was out of reach, so the clamped-output change landed without its two new pngs and with ~9 tool-row pngs stale
