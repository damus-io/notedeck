# Notedeck Dashboard

A minimal, real-time dashboard for **Notedeck**, built with **egui** and backed by **nostrdb**.

This app renders live statistics from a Nostr database without blocking the UI, using a single long-lived worker thread and progressive snapshots. No loading screens, no spinners—data fades in as it’s computed.

## What it does (today)

* Counts **total notes** in the database
* Shows **top N event kinds** as a horizontal bar chart
* Refreshes automatically on a fixed interval
* Streams intermediate results to the UI while scanning
* Keeps previous values visible during refreshes

## Architecture overview

### UI thread

* Pure egui rendering
* Never blocks on database work
* Reacts to snapshots pushed from the worker
* Requests repaints only when new data arrives

### Worker thread

* Single persistent thread
* Runs one scan per refresh
* Emits periodic snapshots (~30ms cadence)
* Uses a single `nostrdb::Transaction` per run
* Communicates via `crossbeam_channel`

### Data flow

```
UI ── Refresh cmd ──▶ Worker
UI ◀─ Snapshot msgs ◀─ Worker
UI ◀─ Finished msg  ◀─ Worker
```

## Code layout

```
src/
├── lib.rs      # App entry, worker orchestration, refresh logic
├── ui.rs       # egui cards, charts, and status UI
└── chart.rs    # Reusable horizontal bar chart widget
```

### `chart.rs`

* Custom horizontal bar chart
* Value labels, hover tooltips, and color palette
* Designed to be generic and reusable

### `lib.rs`

* Implements `notedeck::App`
* Owns worker lifecycle and refresh policy
* Handles snapshot merging and UI state

### `ui.rs`

* Card-based layout
* Totals view + kinds bar chart
* Footer status showing freshness and timing

## Design goals

* **Zero UI stalls**
  Database scans never block rendering.

* **Progressive feedback**
  Partial results are better than spinners.

* **Simple concurrency**
  One worker, one job at a time, explicit messaging.

* **Low ceremony**
  No async runtime, no task pools, no state machines.

## Non-goals (for now)

* Multiple workers
* Historical comparisons
* Persistence of dashboard state
* Configuration UI
* Fancy animations

## Requirements

* Rust (stable)
* `egui`
* `notedeck`
* `nostrdb`
* `crossbeam-channel`

This crate is intended to be built and run as part of a Notedeck environment.

## Status

Early but functional.
The core threading, snapshotting, and rendering model is in place and intentionally conservative. Future changes should preserve the “always responsive” property above all else.
