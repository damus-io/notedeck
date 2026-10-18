# scripts/snapshot-test --changed runs only the snapshots a change can render

`--changed` selects whole suites by crate dependency, so one keymap line in
notedeck_chrome re-renders every chrome and dave suite to check three headway pngs.

- headway:headway/will-print-climb — ~9 min: `--changed=autowork --debug` ran the chrome+dave+headway suites for one keymap line and three headway pngs; killed before it finished, the pngs left to headway:notedeck/road-ritual-copper
