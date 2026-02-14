# NIP-34 Git App for Notedeck — Implementation Plan

## What We're Building

A nostr-native git collaboration app for Notedeck, implementing NIP-34. Users will browse repositories, issues, patches, and pull requests — all decentralized over Nostr relays. The UX will feel familiar to GitHub/GitLab users while being fully decentralized.

**MVP (v1):** Read-only. Browse and display repos, issues, patches, PRs, comments, and status.
**v2:** Write path — post comments (kind 1111), open issues, set status events.

## Decisions

- **Validation policy:** Strict drop. Only display spec-compliant events. Dropped events are logged at `tracing::trace!` level with reason and counted via puffin profiler, enabling developer observability without user-facing noise.
- **Authority cache:** Use nostrdb as cache for maintainer lists and resolved state. Subscribe to kind 30617 updates so the cache auto-refreshes when maintainer lists change.
- **Relay abstraction:** Build a thin `GitRelayBackend` trait abstracting relay operations. Implement for `RelayPool` immediately (current codebase). Add `OutboxPool` implementation when PR [#1288](https://github.com/damus-io/notedeck/pull/1288) lands. No hard dependency on outbox.
- **Android/mobile:** Notedeck compiles to Android (Damus Android). All views must support narrow/mobile screen layouts. This is a cross-cutting requirement, not a bolt-on.
- **NIP-22 P/p tags:** Follow NIP-22 "when available" semantics (`nips/22.md:16`). In NIP-34 comment scopes (issues, patches, PRs), root and parent authors are always nostr events with known pubkeys, so `P`/`p` tags are effectively always required. Strict drop applies: reject kind 1111 events missing `P`/`p` when the referenced root/parent is a nostr event.

## Nostrdb Integration Strategy

The git app should leverage [nostrdb-rs](https://github.com/damus-io/nostrdb-rs) as the primary data layer rather than building parallel caches. Key capabilities to use:

### Direct d-tag queries for addressable events
nostrdb's `FilterBuilder::tags(values, tag_char)` supports filtering by any tag character including `'d'`. This means repo lookups by coordinate are efficient single queries, not full scans:
```rust
// Efficient: query repo by coordinate directly in nostrdb
Filter::new()
    .kinds([30617])
    .authors([pubkey])
    .tags(["repo-identifier"], 'd')
    .limit(1)
    .build()
```

### Repo-scoped queries use full NIP-33 coordinates for a-tags
NIP-34 events (issues, patches, PRs) reference their parent repo via `a` tags using the full coordinate format `30617:<pubkey>:<repo-id>` (see `nips/34.md:124`, `nips/34.md:179`). Queries must match this:
```rust
// Correct: full coordinate in a-tag query
let repo_coordinate = format!("30617:{}:{}", hex::encode(pubkey), repo_id);
Filter::new()
    .kinds([1621])
    .tags([repo_coordinate.as_str()], 'a')
    .build()
```

### Functional query combinators
nostrdb provides `fold`, `try_fold`, `find_map`, `any`, `all` — these short-circuit and avoid materializing full result sets:
- **`find_map`** — find latest repo state by `created_at` without loading all versions
- **`try_fold`** — aggregate status events with early exit on authority match
- **`fold`** — build patch series ordering in a single pass
- **`count`** — efficient counts for UI stats (issue count, PR count per repo) without materializing

### App-level validation (not custom filters)
~~`FilterBuilder::custom(closure)` pushes validation into the query itself.~~ **Do not use `custom()` for validation.** nostrdb-rs `filter.rs:427` has `FIXME: THIS LEAKS!` — custom filter closures leak memory. Since filters are rebuilt during navigation and subscription refresh, this would cause unbounded memory growth. Instead:
- Query with standard kind/tag/author filters at the nostrdb level
- Validate NIP-22/NIP-34 compliance in app code after query results are returned
- Log and count dropped events at `tracing::trace!` level

### Async subscription via `wait_for_notes`
`ndb.wait_for_notes(sub, max)` is async — better than manual poll loops for non-blocking updates. Use with `JobPool` or tokio for frame-safe waiting.

### Subscription lifecycle
Subscriptions are held in struct fields across frames and polled each frame — this is the standard notedeck pattern (see `notedeck_columns/src/timeline/mod.rs:251`). Do NOT recreate subscriptions each frame. Use `FilterStates` for lifecycle management.

### Repo discovery via tag queries (not full-text search)
Repo announcements (kind 30617) have empty content — metadata lives in tags (`name`, `description`, `d`, `clone`, `web`, etc.). `ndb.search()` indexes content/profiles, not tags, so it won't find repos. Discovery must use:
- `kinds([30617])` to list all repos
- Tag-based filtering (`tags([value], 't')` for hashtags) for narrowing
- App-level name/description matching from parsed tag values for search UI

### What nostrdb does NOT handle
- **Replaceable event dedup**: nostrdb stores all versions; app must select newest by `created_at` per coordinate. Use `find_map` with timestamp comparison rather than loading all then sorting.
- **Authority validation**: nostrdb has no concept of maintainer permissions. Status resolver must query maintainer list from 30617 then validate in app code post-query.
- **NIP-specific semantics**: All NIP-22/NIP-34 tag structure validation is app-level (not pushed into `custom()` due to leak).

### Integration pattern per commit

| Commit | nostrdb API used |
|--------|------------------|
| Parsing | `note.tags()` zero-copy iteration, `tag.get_str()`, `tag.get_id()` |
| Addressable coordinator | `tags(['d-value'], 'd')` + `find_map` for newest-by-timestamp |
| Status resolver | `kinds([1630, 1631, 1632, 1633])` + app-level authority check post-query + `fold` for latest-by-timestamp |
| Revision threading | `tags([root_id], 'e')` + `fold` to build ordered series |
| Subscriptions | `subscribe(&filters)`, `wait_for_notes` (async), `poll_for_notes`; held in struct fields across frames |
| Repo list | `kinds([30617])` + app-level name/tag search |
| Issue lists | `kinds([1621])` + `tags(["30617:<pubkey>:<repo-id>"], 'a')` + app-level `fold` count (post-validation) for badges |
| Patch lists | `kinds([1617])` + `tags(["30617:<pubkey>:<repo-id>"], 'a')` + app-level `fold` count (post-validation) for badges |
| PR lists | `kinds([1618])` + `tags(["30617:<pubkey>:<repo-id>"], 'a')` + app-level `fold` count (post-validation) for badges |
| NIP-22 comments | `kinds([1111])` + app-level tag structure validation post-query |

## Reference Architecture

Built following the **Dave app** pattern:
- New crate `notedeck_git` in `crates/`
- Implements `notedeck::App` trait (single `update()` method)
- Registered in Chrome's `NotedeckApp` enum, feature-gated
- State managed in structs (no globals), passed by reference
- Non-blocking UI: `Promise`, `JobPool`, channels for async work

## NIP-34 Event Kinds Covered

| Kind | Purpose |
|------|---------|
| 30617 | Repository announcements (addressable, keyed by `d` tag) |
| 30618 | Repository state — branches/tags (addressable, keyed by `d` tag) |
| 1617 | Patches (<60kb, `git format-patch` output) |
| 1618 | Pull requests (>60kb, pushed to `refs/nostr/[event-id]`) |
| 1619 | PR updates (tip commit changes, NIP-22 threading via `E`/`P` tags) |
| 1621 | Issues (bug reports, features, questions) |
| 1630-1633 | Status: Open / Applied / Closed / Draft |
| 1111 | NIP-22 threaded comments (plaintext, strict tag structure) |
| 10317 | User grasp list (git hosting prefs) |

## Commit Sequence (dependency order)

Each commit is **logically distinct** and **standalone** (removable without breaking others).

### Phase 1: Foundation

1. **Scaffold notedeck_git crate**
   - Empty compilable crate, workspace member, feature gate
   - Cargo.toml with dependencies, lib.rs with module declarations

2. **Define NIP-34 event type structs**
   - Pure data types for all event kinds, docstrings, no parsing logic
   - Blocked by: scaffold

3. **Implement NIP-34 event parsing from nostrdb**
   - `TryFrom`/parse functions, tag extraction, guard clauses (nevernesting)
   - Strict drop: reject events with missing or malformed required tags; log at `tracing::trace!` with reason
   - `a`-tag values are full NIP-33 coordinates: `30617:<pubkey>:<repo-id>`
   - Before implementing, evaluate whether nostrdb upgrades simplify tag access
   - Blocked by: types

4. **Implement GitApp struct with App trait**
   - Main struct, `Router<GitRoute>`, `update()` dispatch
   - State in struct fields — no globals, no thread-locals
   - Blocked by: scaffold

### Phase 2: Domain Logic + Validation

5. **Addressable event coordinator for repos (30617/30618)**
   - Deduplicate by `(kind, pubkey, d-tag)` coordinate, select newest by `created_at`
   - Without this, UI shows duplicate/stale repos
   - Kind 30618 with empty refs = "stop tracking state", not missing data
   - Check if nostrdb handles addressable replacement natively before building custom
   - Blocked by: parsing

6. **Status resolver with maintainer authority engine**
   - Resolve canonical status from kinds 1630-1633 for **patches, PRs, and issues** (`nips/34.md:194`)
   - Latest event by `created_at` wins
   - Valid status authors: the issue/patch/PR author OR any repo maintainer from kind 30617 (`nips/34.md:226`)
   - Revision behavior: status can reference multiple revised patches
   - Authority check is app-level post-query (not `custom()` filter — see leak note)
   - Standalone resolver struct — not embedded in UI code
   - Strict drop: reject status from unauthorized pubkeys; log dropped at trace level
   - Blocked by: addressable coordinator, parsing

7. **Patch/PR revision threading and tip resolution model**
   - Patch series: `t=root` marks the series root patch; `t=root-revision` marks the first patch of a **revision** (`nips/34.md:92`, `nips/34.md:95`) — these are distinct concepts
   - Patches in a series use NIP-10 reply tags for ordering
   - PR updates via kind 1619 change the tip commit
   - Resolve canonical tip/root per series/PR — prevents drift and double-counting
   - Implement `PatchSeries` and `PrThread` domain types
   - Strict drop for malformed threading (missing required tags)
   - Blocked by: parsing

8. **Parser and validator tests** (land BEFORE any UI work)
   - Unit tests for every NIP-34 type parse (valid, malformed, edge cases)
   - NIP-22 kind 1111 tag structure validation: one root ref tag from `{A, E, I}` + `K` tag required; one parent ref tag from `{a, e, i}` + `k` tag required; `P`/`p` tags required in NIP-34 context (root/parent are always nostr events with known authors)
   - `a`-tag coordinate format tests (must be `30617:<pubkey>:<repo-id>`)
   - Addressable event dedup tests (coordinate-keyed selection, empty-refs stop-tracking case)
   - Status resolver tests (authority checking for issues + patches + PRs, timestamp ordering, revision behavior)
   - Patch series tests: `t=root` vs `t=root-revision` distinction, revision ordering
   - `cargo fmt && cargo clippy && cargo test` — no fudging CI
   - Blocked by: parsing, addressable coordinator, status resolver, revision threading

### Phase 3: Plumbing

9. **GitRelayBackend trait + RelayPool implementation**
   - Define `GitRelayBackend` trait abstracting: subscribe, send filter, receive events
   - Implement for current `RelayPool` (works today with existing codebase)
   - Design so `OutboxPool` impl can be added when PR #1288 lands — no schedule risk
   - Blocked by: parsing, app struct

10. **Nostr subscription management for git events**
    - Use FilterStates pattern for subscription lifecycle
    - Subscribe to repo announcements, then repo-specific events when viewing a repo
    - Subscriptions held in struct fields across frames, polled each frame (standard notedeck pattern)
    - Use `GitRelayBackend` for relay communication
    - Blocked by: relay backend, parsing, app struct

11. **Register GitApp in Chrome app router**
    - Add `NotedeckApp::Git` variant at `notedeck_chrome/src/app.rs`
    - Match arm in `App` trait delegation
    - `Chrome::new_with_apps()` initialization
    - Sidebar label/icon match arms — `Other` currently panics at `chrome.rs:857`; must add explicit arm at `chrome.rs:789`
    - Feature gate in workspace Cargo.toml
    - Blocked by: app struct

12. **Git app navigation: icon, sidebar, back-nav**
    - App icon asset for Chrome app switcher (next to Dave icon)
    - In-app back/home navigation
    - Sidebar layout if applicable (repo list sidebar, detail main area)
    - Blocked by: chrome integration

13. **GitHub/GitLab-familiar UX design**
    - Tab layout (Code/Issues/Patches/PRs), information architecture
    - Familiar to GitHub/GitLab users, adapted for decentralized nostr model
    - No server-side merge buttons — status events instead
    - Blocked by: app struct

14. **Responsive mobile/narrow UI layout**
    - Notedeck compiles to Android (Damus Android) — all views must work on narrow screens
    - Follow Dave app's dual-layout pattern: `desktop_ui()` with sidebar + main area, `narrow_ui()` with toggle between views
    - Every view commit must include both desktop and narrow variants
    - Respect `include_input()`/`input_rect()` for soft keyboard visibility on mobile
    - Cross-cutting: this defines the layout scaffolding that all Phase 4 views build on
    - Blocked by: UX design

15. **NIP-34 content rendering (non-kind-1 adaptation)**
    - `render_note_preview` hard-fails non-kind-1 (`notedeck_ui/src/note/contents.rs:79`) — cannot reuse as-is
    - Kind 1111 comments: plaintext only (NIP-22 spec), no markdown rendering
    - Kind 1621 issues: structured content with subject header
    - Kind 1617 patches: `git format-patch` output with diff syntax highlighting
    - Kind 1618 PRs: description text plus structured metadata
    - Evaluate: extend `NoteContents` vs. build `GitContentRenderer`
    - Reuse `ProfilePic`, `padding()`, `hline()` from notedeck_ui
    - Blocked by: UX design, types

### Phase 4: Views

All views must include both desktop and narrow/mobile layouts per commit 14.

16. **Repository list view**
    - Browse/discover repos, `ProfilePic` for maintainer avatars
    - Uses addressable coordinator for dedup — no stale/duplicate repos
    - Discovery via `kinds([30617])` + app-level tag-value search (not `ndb.search()`)
    - Desktop: sidebar repo list + detail pane; Mobile: full-screen list, tap to navigate
    - Blocked by: subscriptions, chrome integration, UX design, parser tests, addressable coordinator, mobile layout

17. **Repository detail view**
    - Full metadata, branch/tag state (kind 30618), tabbed sections for Issues/Patches/PRs
    - Empty-refs handling: display "state tracking paused" not "no branches"
    - Issue/PR/Patch counts via `ndb.fold()` with app-level validation for tab badges (raw `count()` would include malformed events)
    - Mobile: stacked tabs, scrollable metadata
    - Blocked by: repo list

18. **Issue list and detail views**
    - Kind 1621, status indicators from resolver engine (issues included in authority model), labels
    - Query via `tags(["30617:<pubkey>:<repo-id>"], 'a')` for repo-scoped issues
    - Detail view with structured subject+body rendering via content renderer
    - Mobile: full-width issue cards, detail as pushed view
    - Blocked by: repo detail, status resolver, content renderer

19. **Patch list and detail views**
    - Kind 1617, diff rendering via content renderer
    - Series threading via `PatchSeries` domain model (`t=root` for series root, `t=root-revision` for revision start)
    - Show revision history, current vs. previous revisions
    - Status from resolver engine (open/applied/closed/draft)
    - Mobile: horizontal-scroll diff, collapsible patch series
    - Blocked by: repo detail, revision threading, status resolver, content renderer

20. **Pull request list and detail views**
    - Kind 1618/1619, labels, branch info, clone URL
    - Tip resolution via `PrThread` domain model — no double-counting updates
    - Status from resolver engine
    - Mobile: stacked PR metadata, scrollable conversation
    - Blocked by: repo detail, revision threading, status resolver, content renderer

### Phase 5: Social + Polish

21. **NIP-22 threaded comments (kind 1111)**
    - Display threaded comments on issues, patches, and PRs
    - **NIP-22 compliance (validated in app code post-query, not via `custom()`):**
      - Content is plaintext only (`nips/22.md:11`) — no markdown rendering
      - One root ref tag from `{A, E, I}` required + `K` tag required (`nips/22.md:25`)
      - One parent ref tag from `{a, e, i}` required + `k` tag required (`nips/22.md:33`)
      - `P`/`p` tags: required in NIP-34 context — root/parent are always nostr events with known authors (`nips/22.md:16`)
      - Strict drop with trace logging: reject events missing required root/parent ref, K/k, or P/p tags
    - **Must NOT** route through existing note-reply compose path (`notedeck_columns/src/post.rs:87`) which emits kind-1/NIP-10 replies
    - v1: read-only display; v2: dedicated kind 1111 composer
    - Mobile: full-width comment thread, indented replies
    - Blocked by: issues, patches, PRs views

22. **Puffin profiling instrumentation**
    - `profiling::function` on performance-sensitive functions: event parsing, subscription management, list rendering, detail rendering
    - Include drop counters for strict-drop validation (events rejected by parser, status resolver, NIP-22 validator)
    - Verify puffin feature gate works with new crate
    - Test with `cargo run --release --features puffin`
    - Blocked by: subscriptions

23. **Integration tests**
    - End-to-end subscription filter tests, UI smoke tests
    - Verify strict drop works in full pipeline (events logged, not rendered)
    - Verify `a`-tag coordinate queries return correct repo-scoped results
    - `cargo fmt && cargo clippy && cargo test`
    - Blocked by: repo list view (ensures full stack is testable)

## Known Pitfalls

| # | Severity | Pitfall | Mitigation |
|---|----------|---------|------------|
| 1 | Critical | Kind 1111 has strict NIP-22 tag structure — one root ref from `{A,E,I}` + `K` + `P`, one parent ref from `{a,e,i}` + `k` + `p` (P/p required in NIP-34 context); reusing kind-1/NIP-10 reply flow breaks interop | Dedicated validator (app-level, not `custom()`), dedicated composer (v2), never route through `post.rs` |
| 2 | High | Status events can be spoofed — authority depends on maintainer list from 30617 and event author identity | Standalone resolver checks pubkey against maintainer list + issue/patch/PR author (all three, not just patches/PRs) |
| 3 | High | 30617/30618 are addressable/replaceable — without coordinate-keyed dedup, UI shows stale/duplicate repos | Addressable event coordinator with `(kind, pubkey, d-tag)` keying |
| 4 | High | Patch series and PR revision threading is complex — `t=root` (series root) vs `t=root-revision` (revision start) are distinct | Dedicated `PatchSeries`/`PrThread` domain model with correct tag interpretation |
| 5 | High | `a`-tag queries must use full NIP-33 coordinate format `30617:<pubkey>:<repo-id>`, not bare repo ID | Explicit coordinate construction in all repo-scoped queries |
| 6 | High | `FilterBuilder::custom()` leaks memory (`nostrdb-rs filter.rs:427 FIXME: THIS LEAKS!`) | All validation in app code post-query; never use `custom()` in hot paths |
| 7 | Medium | Chrome sidebar panics on unhandled app variants (`chrome.rs:857`) | Explicit match arm for Git app in sidebar rendering |
| 8 | Medium | `render_note_preview` hard-fails non-kind-1 (`contents.rs:79`) | Git-specific content renderer or extended `NoteContents` |
| 9 | Medium | Outbox PR #1288 may slip; current codebase uses `RelayPool` | `GitRelayBackend` trait abstracts relay ops; `RelayPool` impl now, `OutboxPool` impl later |
| 10 | Medium | `ndb.search()` indexes content/profiles, not tags; repo announcements have empty content | Repo discovery via `kinds([30617])` + app-level tag-value matching |
| 11 | Low | Silent strict drop makes interop debugging difficult | Log dropped events at `tracing::trace!` with reason; count in puffin profiler |

## Coding Standards Applied

All work follows the documented notedeck requirements:
- Logically distinct, standalone commits — each removable without impact
- Nevernesting (early returns/guard clauses)
- No globals, no Mutexes (`Rc<RefCell<>>`, `Promise`, `JobPool`)
- Never block the render loop
- Docstring coverage on all new code
- `cargo fmt && cargo clippy && cargo test` before every commit
- Reuse existing notedeck_ui components before creating new
- No vendored code
- Puffin-instrumented performance-sensitive paths
- Simplicity: small, human-reviewable commits
- Cherry-pick to preserve authorship when incorporating external work
- Rebase fixes into original commits within same PR

## Notedeck Components to Reuse

| Component | Location | Use Case |
|-----------|----------|----------|
| `AppContext` / `AppResponse` | `notedeck/src/app.rs` | Framework integration |
| `Router<R>` | `notedeck/src/route.rs` | Stack-based navigation with animations |
| `FilterStates` / `UnifiedSubscription` | `notedeck/src/filter.rs` | Relay subscription lifecycle (held across frames) |
| `JobPool` / `JobCache` | `notedeck/src/jobs/` | Async work offloading (non-blocking) |
| `ProfilePic` / `ProfilePreview` | `notedeck_ui/` | Author/maintainer display |
| `padding()`, `hline()`, `search_input_box()` | `notedeck_ui/` | UI layout helpers |
| `AnimationHelper` | `notedeck_ui/src/anim.rs` | Hover/expand interactions |
| `TextureState<T>` | `notedeck/src/imgcache.rs` | Async loading pattern (Pending/Error/Loaded) |

**Cannot reuse as-is:**
- `NoteView` / `NoteContents` — hard-fails on non-kind-1 events; needs adaptation or replacement for NIP-34 kinds
- `post.rs` reply compose — emits kind-1/NIP-10, not kind-1111/NIP-22; must not use for git comments
- `ndb.search()` — indexes content/profiles, not tags; cannot discover repos by name
