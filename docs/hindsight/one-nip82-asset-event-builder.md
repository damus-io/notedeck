# One NIP-82 asset event builder for the release CLI and the updater's tests

The release CLI and the updater's `test_helpers` each hand-roll kind 3063 events,
so they drift; the helpers wrote no `name` tag the CLI always publishes.

- headway:notedeck/same-surprise-bunker — ~10 min: the asset picker matches on `name`, which the test helpers didn't write; threaded a `name` argument through both helpers and eight call sites, and still nothing ties them to what `notedeck_release::build_asset_event` emits
