# notedeck_cashflow

A personal finance dashboard for Notedeck: where money comes from and goes,
progress over time, goals tracked as Headway cards, and an optional Adventure
view that turns better cashflow into XP.

This crate is the UI only. The ledger engine is `crates/ledgerdb` and the
importers, rules and nostr vault are `crates/cashflow` (whose `cashflow` CLI
shares the app's `$CASHFLOW_DIR`).

Run it with:

    cargo run -p notedeck_chrome --features cashflow

## Layout

- `src/view.rs` — `View`, the router: one variant per screen.
- `src/views/` — one module per view, each `fn ui(cx: &mut ViewCx, ui)`.
- `src/ui/palette.rs` — `Palette`, the semantic colors every view draws with,
  derived from the host's `ColorTheme` and held in app state.
- `src/ui/nav.rs` — the view nav: a rail on wide screens, a tab strip on
  narrow ones.
- `tests/common/` — the snapshot harness every UI test builds on.

Snapshots run under lavapipe: `scripts/snapshot-test -p=notedeck_cashflow`.
