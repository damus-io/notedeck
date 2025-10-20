# Notedeck Agent Development Overview

This document captures the current architecture, coding conventions, and design patterns across the Notedeck repository to help new agent-driven experiences slot cleanly into the existing codebase.

## Repository Topology

- **`crates/notedeck`** – Core framework: application host (`Notedeck`), shared services (`AppContext`, `Accounts`, caches, persistence, localization).
- **`crates/notedeck_chrome`** – Container UI that boots Notedeck, manages the application switcher/sidebar, and wires apps into the main window.
- **`crates/notedeck_columns`** – Primary “Damus” client: timelines, decks/columns, routing, multi-relay subscription management.
- **`crates/notedeck_ui`** – Reusable egui widgets (`NoteView`, media renderers, profile components) and UI utilities.
- **`crates/notedeck_dave`** – Dave AI assistant showcasing agent-style tooling, streaming responses, and custom rendering.
- **`crates/tokenator`** – Text token utility library used by other crates.

## Core Abstractions & Patterns

### Application Host

- **`App` trait** (`crates/notedeck/src/app.rs`): Apps implement `update(&mut self, &mut AppContext, &mut egui::Ui) -> AppResponse` to drive egui rendering and signal high-level actions (`AppAction` for route changes, chrome toggles, etc.).
- **`Notedeck` struct** (`crates/notedeck/src/app.rs`) owns global resources—NostrDB connection, caches, relay pool, accounts, zaps, localization, clipboard, frame history—and injects them through `AppContext`.
- **`AppContext`** (`crates/notedeck/src/context.rs`) is the dependency hub handed to every app update. It exposes mutable handles to services (database, caches, relay pool, account state, localization, job pool, settings, wallet) so apps stay decoupled from the host.
- **`AppResponse`** carries optional actions and drag targets; chrome inspects it to react to app-level intent.

### UI Container & Navigation

- **Chrome shell** (`crates/notedeck_chrome/src/chrome.rs`): wraps multiple `App` instances, draws sidebar navigation, and forwards egui `update` passes to the active app.
- **`NotedeckApp` enum** (`crates/notedeck_chrome/src/app.rs`) defines the shipping app roster (Columns/Damus, Dave, others) and provides constructors for wiring new apps.
- **Preview system & theming** (`crates/notedeck_chrome/src/preview.rs`, `crates/notedeck_chrome/src/theme.rs`) centralize look-and-feel, font loading, and debug previews.

### Nostr Data & Networking

- **Database**: `nostrdb::Ndb` is the primary storage/query engine. Transactions are short-lived (`Transaction::new`) and most reads flow through caches.
- **Caches**:
  - `NoteCache` (NIP-10/thread metadata),
  - `Images` (image/GIF cache),
  - `UnknownIds` (tracks pubkeys/notes discovered via tags),
  - `JobsCache` (Columns-specific async job state).
- **Relay management**: `enostr::RelayPool` is shared in `AppContext`. Apps enqueue filters, process `RelayEvent`s, and update timelines.
- **Subscriptions**: Columns crate layers `Subscriptions`, `MultiSubscriber`, and `TimelineCache` to fan out relay queries per column. Unknown IDs are resolved lazily and retried until satisfied.
- **Debouncing & persistence**: `TimedSerializer` + `Debouncer` persist settings/state without hammering the filesystem (`crates/notedeck/src/timed_serializer.rs`).

### UI Composition

- **Immediate-mode UI**: All apps render with `egui`, respecting the host’s `Context` for theming and input.
- **Shared components** (`crates/notedeck_ui`):
  - `NoteView` bundles author header, body, media, and action bar with configurable `NoteOptions`.
  - Profile widgets (`ProfilePic`, `ProfilePreview`), media viewers, mention chips, and timeline helpers keep rendering consistent.
- **Columns-specific layout**: `Damus` app (`crates/notedeck_columns/src/app.rs`) manages decks, per-column routers, timeline hydration, and keyboard navigation. It uses `StripBuilder` and custom panels for multi-column flows.
- **Chrome** handles responsive breakpoints (e.g., `ui::is_narrow`) to switch layouts for mobile widths.

### Async & Background Work

- **`JobPool`** (`crates/notedeck/src/job_pool.rs`) provides a lightweight thread pool returning Futures via `tokio::sync::oneshot`, useful for CPU-bound tasks without blocking the UI.
- **Dave async**: Streams AI tokens through channels, spawns tasks with `tokio::spawn`, and updates the UI as chunks arrive—see `crates/notedeck_dave/src/lib.rs`.
- **Relay events**: Columns polls `RelayPool::try_recv()` inside the egui loop, translates network activity into timeline mutations, and schedules follow-up fetches (e.g., `timeline::poll_notes_into_view`).

### Localization, Styling, Persistence

- **Localization**: `tr!`/`tr_plural!` macros (documented in `crates/notedeck/DEVELOPER.md`) normalize strings into Fluent keys. `LocalizationManager` caches translations; locale is saved via `SettingsHandler`.
- **Themes & fonts**: `ColorTheme`, `NamedFontFamily`, and theme builders ensure consistent typography and support OLED dark mode.
- **Settings & tokens**: `SettingsHandler` stores theme, zoom, locale, and textual toggles; `TokenHandler` persists auth tokens safely.

### Dave Agent Patterns (Template for Future Agents)

- **Structured tool system** (`crates/notedeck_dave/src/tools.rs`): Defines tool metadata, JSON argument parsing, and execution into typed responses. Great reference for agent capabilities (search, present notes).
- **Streaming UI**: Uses `mpsc` channels to surface streaming AI output while continuing to render frames (`crates/notedeck_dave/docs/developer-guide.md`).
- **Custom rendering**: Demonstrates embedding WebGPU callbacks for 3D avatars while remaining within egui’s lifecycle.

## Coding Conventions & Practices

- Rust 2021, edition-lints are strict; clippy `disallowed_methods` is denied at crate root to enforce API hygiene (`crates/notedeck/src/lib.rs`).
- Prefer module-level organization over monolithic files; each feature (accounts, decks, timelines, media) lives in its own module tree.
- Use `tracing` macros for structured logging and `profiling` scopes where hot paths exist (Columns’ relay/event loop).
- UI code embraces egui idioms: builder chains, closures returning `Response`, `ui.vertical`/`horizontal` for layout.
- Persist state via `TimedSerializer::try_save` to avoid blocking the frame; batch mutations with `SettingsHandler::update_batch`.
- Tests live alongside modules (e.g., `JobPool`), often using `#[tokio::test]` when async behavior is involved.
- Localization updates: run `python3 scripts/export_source_strings.py` after changing user-facing strings; translators rely on the generated Fluent files.

## Integrating New Agents

1. **Prototype as an App**: Implement the `App` trait, using `AppContext` to read from `Ndb`, inspect accounts, schedule jobs, and access localization.
2. **Register in Chrome**: Add a variant to `NotedeckApp`, supply icon/label metadata, and hook it into the sidebar.
3. **Leverage shared UI**: Reuse `notedeck_ui` components (note previews, media viewers) for consistency. Compose with `NoteContext` when rendering Nostr events.
4. **Relay access pattern**: Subscribe to the relevant Nostr kinds through `RelayPool`, mirroring Columns’ subscription helpers or Dave’s targeted queries.
5. **State & persistence**: Store lightweight view state in your app struct; use `TimedSerializer` only if persisting user preferences.
6. **Localization & theming**: Wrap strings with `tr!`, respect `ctx.style()` for colors/fonts, and support narrow layouts.

## Reference Material

- `README.md` for project overview and crate map.
- `crates/notedeck/DEVELOPER.md` for core architecture, localization, caching.
- `crates/notedeck_chrome/DEVELOPER.md` for container lifecycle and theming.
- `crates/notedeck_columns/DEVELOPER.md` for timeline/deck architecture.
- `crates/notedeck_dave/docs/*.md` for agent-style tooling and streaming patterns.
- `crates/notedeck_ui/docs/components.md` for reusable widgets.

## Notedeck Coding Patterns

1. Please make all commits logically distinct.
2. Please make all commits standalone (i.e. so that they can be readily removed tens of commits later without impact the rest of the code).
3. Please set up code for performance profiling utilizing puffin (e.g. `cargo run --release --features puffin`).
4. Related to Puffin & performance profiling, for code suspected of impacting performance, carefully consider adding performance profiling attributes such as e.g. profiling::function in order to see functions performance in the profiler. 



Use this guide as a launchpad when extending Notedeck with new agents or protocol features. It highlights where to attach new functionality without duplicating existing infrastructure.
