# NIP-34 Git App for Notedeck — Implementation Plan

## What We're Building

A nostr-native git collaboration app for Notedeck implementing NIP-34. Browse repositories, issues, patches, and pull requests — decentralized over Nostr relays. The UX follows GitHub/GitLab patterns. Git collaboration is a solved problem; we're decentralizing the transport.

**MVP:** Read-only. Browse repos, issues, patches, PRs, comments, status.
**Post-MVP:** Write path (comments, issues, status). CLI tool.

## Prior Art

Existing NIP-34 implementations we should study and learn from:

| Project | Type | Notes |
|---------|------|-------|
| [gitworkshop.dev](https://gitworkshop.dev/) | Web (Svelte) | Primary web client. QueryCentre hub, isomorphic-git for diffs in-browser |
| [ngit-cli](https://codeberg.org/DanConwayDev/ngit-cli) | CLI (Rust) | v2.0.1, 850+ commits. `nostr://` remote, `git-remote-nostr` helper |
| [Gitplaza](https://codeberg.org/dluvian/gitplaza) | Desktop (Rust/Iced) | Closest to what we're building. 5 views: repo, patch, issue, profile, inbox |
| [gitstr](https://github.com/fiatjaf/gitstr) | CLI (Go) | Minimal. fiatjaf uses it for nak/go-nostr |

**Key relay:** `relay.ngit.dev` — public ngit-relay (GRASP protocol reference impl). Most NIP-34 activity concentrates here. Repos specify their relays in the `relays` tag of kind 30617.

**Lessons from prior art:**
- Gitplaza reads patch descriptions from the `description` tag, not letter content
- `nostr://` URL format (`nostr://<npub-or-nip05>/<repo-id>`) is the emerging standard for clone URIs
- ngit is dogfooded (developed using itself) — proves the workflow works
- Patch vs PR model tension exists — NIP-34 supports both, clients must handle both
- Discovery depends on relay connectivity — no central index

## Open Question

**AsyncLoader pattern:** jb55 said "we have a new AsyncLoader pattern" in review. No type named `AsyncLoader` exists anywhere — not in notedeck, nostrdb, or nostrdb-rs (checked all branches, stashes, PRs). nostrdb-rs has `SubscriptionStream` (`future.rs`) which wraps `poll_for_notes()` into a `futures::Stream`, and `wait_for_notes()` which uses it — but jb55 explicitly said the plan's use of `wait_for_notes` was "incorrect" and referenced AsyncLoader as something different. He also said "we should standardize this into nostrdb-rs somehow", confirming it's aspirational. **Need to clarify with jb55 what he envisions.** Until then, use `poll_for_notes()` + app-level state enum (the established pattern in messages app, contacts, timelines) and adapt when it materializes.

---

## Decisions

- **Validation:** Strict drop. Only display spec-compliant events. Log drops at `tracing::trace!`.
- **Relay access:** Use `ctx.remote` (RemoteApi / ScopedSubApi) — the new standard after outbox merge (#1303). No custom relay abstraction.
- **Async work:** `poll_for_notes()` for subscriptions in the frame loop. `JobCache`/`JobPool` for heavy CPU/IO.
- **Relay discovery:** Hardcode `relay.ngit.dev` as default git relay (like livestream app hardcodes streaming relays). Once a repo is loaded, use its `relays` tag for scoped subscriptions.
- **Mobile:** All views must support `is_narrow()`. Cross-cutting, not bolt-on.
- **No `FilterBuilder::custom()`:** Leaks memory (`filter.rs:427 FIXME`). All NIP validation in app code post-query.

---

## The Hard Parts

These are the technically risky areas that need design attention, not just bullet points.

### 1. Diff Rendering

Patches (kind 1617) contain `git format-patch` output — unified diff text. Rendering this well in egui is the single hardest UI problem in the app.

**Parsing:** Use the [`patch`](https://docs.rs/patch/latest/patch/) crate (Rust, zero-copy, `nom`-based). It parses unified diffs into `Patch` → `Hunk` → `Line::Add/Remove/Context` — maps directly to rendering. Gitplaza uses [`unidiff`](https://docs.rs/unidiff/latest/unidiff/) (also viable, heavier API).

**Rendering:** Use `LayoutJob` + `TextFormat` with per-line `background` colors. This exact pattern already exists in `PostBuffer::to_layout_job()` (`notedeck_columns/src/post.rs:399`) for coloring mentions. Monospace font (Inconsolata) already configured via `NotedeckTextStyle::Monospace`.

```rust
// Per-line coloring via LayoutJob (existing notedeck pattern)
for line in patch.hunks.iter().flat_map(|h| &h.lines) {
    let (bg, fg) = match line {
        Line::Add(_)     => (GREEN_BG, GREEN_FG),
        Line::Remove(_)  => (RED_BG, RED_FG),
        Line::Context(_) => (TRANSPARENT, GREY),
    };
    job.append(&format!("{line}\n"), 0.0, TextFormat {
        font_id: FontId::monospace(13.0),
        color: fg, background: bg, ..Default::default()
    });
}
```

**MVP scope:** Colored lines only (green additions, red deletions, grey context). File headers as collapsible sections. Horizontal scroll for long lines.

**NOT MVP:** No split (side-by-side) view. No inline commenting. No syntax highlighting within diff lines (would require enabling `egui_extras` `syntect` feature — significant binary size increase).

### 2. Subscription Fan-out

When viewing a repo, we need events from multiple kinds. Using `ScopedSubApi`:

```rust
// One owner per repo view instance
let owner = SubOwnerKey::builder("git-repo-view").with(repo_coord).finish();

// Subscribe to repo-scoped events in a single filter group where possible
let filters = vec![
    // Repo announcement + state
    Filter::new().kinds([30617, 30618]).authors([pubkey]).tags([d_tag], 'd').build(),
    // Issues, patches, PRs for this repo
    Filter::new().kinds([1621, 1617, 1618]).tags([repo_a_tag], 'a').build(),
    // Status events (optional a-tag for relay filter efficiency per NIP-34)
    Filter::new().kinds([1630, 1631, 1632, 1633]).tags([repo_a_tag], 'a').build(),
];

ctx.remote.scoped_subs(ctx.accounts).ensure_sub(
    ScopedSubIdentity::account(owner, sub_key),
    SubConfig {
        relays: RelaySelection::Explicit(repo_relays),  // from 30617 relays tag
        filters,
        use_transparent: false,
    },
);

// Comments loaded lazily when viewing a specific issue/patch/PR
// (separate sub with same owner, torn down on navigate-back)
```

**Lifecycle:** `ensure_sub` on view open, `drop_owner` on view close. Shared subs ref-counted by the runtime.

**Comment counts:** Don't query per-item on list views. Either:
- Show comment counts only on detail views (simplest)
- Or batch-query kind 1111 for all visible items and count client-side

### 3. First-Run / Repo Discovery

Users need a way to find repos. Two approaches for MVP:
- **Default relay:** Pre-configure `relay.ngit.dev` (like livestream app hardcodes streaming relays) and query `kinds([30617])` for a browsable list
- **Manual add:** Paste a `naddr` or `nostr://` URI to subscribe to a specific repo

Follow-based discovery (repos from people you follow) is a nice-to-have.

---

## App Behavior — Views

### What the GUI deliberately omits (NIP-34 limitations)

These are GitHub/GitLab features users might expect but that NIP-34 doesn't support. The GUI should not fake these with half-measures — better to omit cleanly than confuse users.

| Missing feature | Why | What we show instead |
|----------------|-----|---------------------|
| Merge button | No server-side merge. Git servers are dumb relays. | Status badge updates when maintainer publishes kind 1631 (Applied) |
| Approve / Request changes | No review workflow semantics | Comments only (kind 1111). All comments are equal. |
| CI/CD checks | No CI integration in NIP-34 | Nothing. No checks tab. |
| Code browser / file tree | No file tree kind | Clone URLs on Info tab. User clones locally. |
| Assignees | No assignment concept | Not shown. Author is the only attributed identity. |
| Milestones / Projects | No structured project management | Not shown |
| Fork graph | Repos are independent. No parent/child relationship. | `t=personal-fork` tag shown as a badge if present |
| Server-side search | No relay-side search for NIP-34 events | Client-side filtering by tag values |
| Labels CRUD | Labels are `t` tags on events — immutable after publish | Display labels read-only. No create/edit/delete. |

### Navigation

**Routes** (stack-based via `Router<GitRoute>`):
```
RepoList                          — browse/search repos
RepoDetail(RepoCoord)             — repo overview with tabs
IssueDetail(RepoCoord, NoteId)    — single issue + comments
PatchDetail(RepoCoord, NoteId)    — single patch/series + diff + comments
PrDetail(RepoCoord, NoteId)       — single PR + comments
```

Issue/patch/PR lists are tabs within `RepoDetail`, not separate routes.

**Desktop:** Left sidebar with repo list (persistent). Main area shows selected repo's tab content. Click an item in a tab list to push a detail route.

**Mobile:** Full-screen stack. RepoList → RepoDetail (with tabs) → Item detail. Back button to pop.

### Repository List

```
┌──────────────────────────────────────────────┐
│ [Search repos...]                   [+ Add]  │
├──────────────────────────────────────────────┤
│ 👤 jb55 / notedeck                           │
│ The best nostr client                        │
│ Updated 1d ago                               │
├──────────────────────────────────────────────┤
│ 👤 fiatjaf / nak                             │
│ the nostr army knife                         │
│ Updated 3d ago                               │
└──────────────────────────────────────────────┘
```

- Maintainer avatar (`ProfilePic`) + repo name (from `name` tag on 30617)
- Description (from `description` tag)
- Last updated timestamp
- Search: client-side filter by name/description tag values (not `ndb.search()`)
- Sort: most recently updated first

### Repository Detail (with tabs)

```
┌──────────────────────────────────────────────┐
│ ← Back                                       │
│ 👤 jb55 / notedeck                           │
│ The best nostr client                        │
│                                              │
│ [Issues 12] [Patches 3] [PRs 1] [Info]       │
├──────────────────────────────────────────────┤
│                                              │
│ (tab content — e.g. issue list below)        │
│                                              │
│ ● Fix crash on Android startup               │
│   opened 2h ago by alice                     │
│                                              │
│ ● Add dark mode support                      │
│   opened 1d ago by bob                       │
│                                              │
│ ○ Improve relay handling                      │
│   closed 3d ago by charlie                   │
│                                              │
└──────────────────────────────────────────────┘
```

**Tabs:**
- **Issues** (default) — kind 1621 list with Open/Closed filter
- **Patches** — kind 1617 list with Open/Applied/Closed/Draft filter
- **PRs** — kind 1618 list with same status filters
- **Info** — clone URLs, web URL, relays, maintainer list, branch/tag state (30618)

**List items show:** status icon, title, author, timestamp. Labels from `t` tags shown as colored badges when present.

**Status icons:**

| Kind | Icon | Color | Label |
|------|------|-------|-------|
| 1630 | `●` | Green | Open |
| 1631 | `✓` | Blue | Applied |
| 1632 | `○` | Grey | Closed |
| 1633 | `◌` | Yellow | Draft |

Status resolved by authority engine: only the item's author or a repo maintainer can set status.

### Issue Detail

```
┌──────────────────────────────────────────────┐
│ ← Issues                                     │
│ Fix crash on Android startup                 │
│ ● Open · opened 2h ago by alice              │
├──────────────────────────────────────────────┤
│ 👤 alice · 2h ago                            │
│ The app crashes on launch when the relay     │
│ list is empty.                               │
│                                              │
│ 👤 bob · 1h ago                              │
│ I can reproduce. Null pointer in relay init. │
│                                              │
│   └─ 👤 alice · 45m ago                      │
│      Thanks, pushing a fix.                  │
└──────────────────────────────────────────────┘
```

- Issue body (Markdown — NIP-34 spec says issues SHOULD use Markdown) as first "comment"
- Threaded NIP-22 comments (kind 1111) with indentation for replies
- Comments are plaintext only (NIP-22 spec — distinct from issue body which is Markdown)
- v1: read-only. v2: comment composer (kind 1111, NOT kind-1/NIP-10)

### Patch Detail

```
┌──────────────────────────────────────────────┐
│ ← Patches                                    │
│ Refactor relay connection pool               │
│ ● Open · by alice · 4h ago · v2              │
├──────────────────────────────────────────────┤
│ [Description] [Diff] [Comments]              │
├──────────────────────────────────────────────┤
│                                              │
│  src/relay.rs                                │
│  @@ -42,7 +42,9 @@ impl RelayPool {          │
│     fn connect(&mut self) {                  │
│  -      self.socket = connect_raw();         │
│  +      let config = Config::default();      │
│  +      self.socket = connect_with(config);  │
│     }                                        │
│                                              │
└──────────────────────────────────────────────┘
```

- Sub-tabs: Description (commit message), Diff (see "Diff Rendering" above), Comments
- Series display: if multi-patch (`t=root`), show ordered list of patches in series
- Revision selector if multiple revisions exist (`t=root-revision`)
- Mobile: horizontal scroll for diff lines

### PR Detail

Same layout as issue detail, plus:
- Branch labels (source → target) from `branch-name`/`merge-base` tags
- Clone URL(s) from required `clone` tag(s) + fetch command for `refs/nostr/<event-id>`
- Tip commit from required `c` tag
- Update timeline from kind 1619 events (each updates `c` tag with new tip)
- No merge button — status events only (kind 1631 = Applied)

---

## NIP-34 Event Kinds and Required Tags

| Kind | Purpose | Required tags | Content |
|------|---------|---------------|---------|
| 30617 | Repository announcement | `d` | — (metadata in tags: `name`, `description`, `clone`, `web`, `relays`, `maintainers`) |
| 30618 | Repository state | `d` | — (refs in tags: `refs/heads/<name>`, `refs/tags/<name>`, `HEAD`) |
| 1617 | Patches | `a` (repo coord), `p` (repo owner) | `git format-patch` output (<60kb) |
| 1618 | Pull requests | `a`, `c` (tip commit), `clone` (1+), `subject` | PR description (Markdown) |
| 1619 | PR updates | `a`, `E` (PR event), `P` (PR author), `c`, `clone` | — |
| 1621 | Issues | `a`, `p` (repo owner) | Markdown. Optional `subject`, `t` (labels) |
| 1630 | Status: Open | `e` (item id, "root"), `p` (root author) | — |
| 1631 | Status: Applied | `e`, `p` | Optional: `merge-commit`, `applied-as-commits` |
| 1632 | Status: Closed | `e`, `p` | — |
| 1633 | Status: Draft | `e`, `p` | — |
| 1111 | NIP-22 comments | Root ref (`A`/`E`/`I` + `K`), parent ref (`a`/`e`/`i` + `k`), `P`/`p` | Plaintext only |

**Relay routing:** Patches, PRs, and issues SHOULD be sent to the relays in the repo's 30617 `relays` tag.

**Status authority:** Most recent status event (by `created_at`) from either the item author or a repo maintainer is valid.

---

## Implementation Plan — Vertical Slices

Each slice delivers a working feature end-to-end. You can demo after each slice.

### Slice 1: See a Repo (commits 1-4)

Goal: Launch the git app, see a list of repos, tap one to see its metadata.

**1. Scaffold `notedeck_git` crate**
- Workspace member, feature gate, Cargo.toml, lib.rs
- `GitApp` struct implementing `notedeck::App` trait
- `Router<GitRoute>` with `RepoList` and `RepoDetail` routes
- Register as `NotedeckApp::Git` in chrome (explicit sidebar match arm)

**2. Parse kind 30617 (repo announcements)**
- `RepoAnnouncement` struct: name, description, clone URLs, web URL, relays, maintainers, d-tag
- Parse from nostrdb `Note` via tag iteration
- Addressable dedup: select newest by `created_at` per `(kind, pubkey, d-tag)` coordinate
- Unit tests for parse + dedup (valid, malformed, multiple versions)

**3. Subscribe to repos and render list**
- Use `ctx.remote.scoped_subs()` to subscribe to `kinds([30617])` on default relay
- Poll with `ndb.poll_for_notes()` each frame
- Render repo cards: avatar, name, description, timestamp
- Client-side search filter
- Both desktop (sidebar list) and mobile (full-screen list) layouts

**4. Repo detail — Info tab**
- Push `RepoDetail` route on card click
- Show clone URLs, web URL, relays, maintainer list
- Parse kind 30618 for branch/tag state (if available)
- Handle empty refs: show "State tracking paused"

### Slice 2: Issues (commits 5-7)

Goal: See issues for a repo, read an issue with comments.

**5. Parse kind 1621 (issues) + status resolver**
- `Issue` struct: subject (from `subject` tag), body (content — Markdown), author, labels (from `t` tags), required `a` tag (repo coord) + `p` tag (repo owner)
- Status resolver: query kinds 1630-1633 by `e` tag (item id, marked "root") + `p` tag (root author), check authority (item author or maintainer), latest `created_at` wins
- Unit tests for status resolution (authorized, unauthorized, ordering)

**6. Issue list tab**
- Add Issues tab to repo detail (subscribe to `kinds([1621])` scoped by repo `a`-tag)
- Open/Closed filter tabs with counts
- Status icons, title, author, timestamp, labels

**7. Issue detail + NIP-22 comments**
- Push `IssueDetail` route on row click
- Render issue body + threaded kind 1111 comments
- NIP-22 validation: require root ref tag (`A`/`E`/`I` + `K`), parent ref tag (`a`/`e`/`i` + `k`), `P`/`p` tags in NIP-34 context
- Lazy-load comments (separate sub scoped to this issue, same owner key)
- Comment threading by parent tag → indentation

### Slice 3: Patches (commits 8-10)

Goal: See patches for a repo, view a diff.

**8. Parse kind 1617 (patches) + revision threading**
- `Patch` struct: content (`git format-patch` output), subject, author
- `PatchSeries`: `t=root` marks series root, `t=root-revision` marks revision start — distinct concepts
- NIP-10 reply tags for ordering patches within a series
- Unit tests for series ordering and revision detection

**9. Diff parser + renderer**
- Parse unified diff format: file headers, `@@` hunks, `+`/`-`/` ` lines
- `DiffView` widget: monospace text with colored spans, collapsible file sections
- Horizontal scroll for long lines
- Fallback: raw monospace text if parsing fails

**10. Patch list tab + detail view**
- Add Patches tab with status filter (Open/Applied/Closed/Draft)
- Patch detail: Description / Diff / Comments sub-tabs
- Series display (ordered list, collapsible)
- Revision selector for multi-revision patches

### Slice 4: Pull Requests (commits 11-12)

Goal: See PRs, view PR details with update timeline.

**11. Parse kind 1618/1619 (PRs + updates)**
- `PullRequest` struct: subject (required), description (content, Markdown), clone URLs (required, 1+), tip commit `c` (required), branch-name, merge-base
- `PrThread`: kind 1619 updates change the tip commit (`c` tag) and require `E` (PR event) + `P` (PR author)
- Resolve canonical tip per PR (latest 1619 by `created_at`)

**12. PR list tab + detail view**
- Add PRs tab with status filter
- PR detail: branch labels, clone/fetch command, update timeline, comments
- Reuse comment threading from slice 2

### Slice 5: Polish (commits 13-14)

**13. Puffin profiling + integration tests**
- Instrument parsing, subscription management, list rendering
- Drop counters for strict-drop validation
- End-to-end tests: subscribe → parse → render

**14. Labels, sort options, UX refinements**
- Label rendering (colored badges from `t` tags) on issue/patch/PR lists
- Sort options: newest, recently updated
- Loading states (check `sub_eose_status` for spinner vs content)

---

## Nostrdb Integration

**Queries:**
```rust
// Repo by coordinate
Filter::new().kinds([30617]).authors([pubkey]).tags([d_tag], 'd').limit(1).build()

// Repo-scoped items (issues, patches, PRs)
let coord = format!("30617:{}:{}", hex::encode(pubkey), repo_id);
Filter::new().kinds([1621]).tags([coord.as_str()], 'a').build()

// Status events (reference item by e-tag, optionally scoped by a-tag for efficiency)
Filter::new().kinds([1630, 1631, 1632, 1633]).tags([item_id], 'e').build()

// Comments on an item
Filter::new().kinds([1111]).tags([item_id], 'E').build()
```

**Combinators:** `find_map` for latest-by-timestamp addressable dedup. `fold` for building patch series ordering. App-level validation post-query (never `custom()`).

**Subscription pattern:** `ndb.subscribe()` + `poll_for_notes()` each frame. Subscriptions held in struct fields, never recreated per frame.

---

## Relay Integration (post-outbox merge)

Use `ctx.remote` (RemoteApi) exclusively. No custom relay abstraction needed.

```rust
// Declare a scoped subscription
let owner = SubOwnerKey::builder("git-repo").with(repo_coord).finish();
let key = SubKey::builder("repo-events").with(repo_coord).finish();

ctx.remote.scoped_subs(ctx.accounts).ensure_sub(
    ScopedSubIdentity::account(owner, key),
    SubConfig {
        relays: RelaySelection::Explicit(repo_relays),  // from 30617 relays tag
        filters,
        use_transparent: false,
    },
);

// On view close
ctx.remote.scoped_subs(ctx.accounts).drop_owner(owner);
```

For repo discovery (before we know a repo's relays), use `RelaySelection::Explicit` with default git relays:

```rust
const GIT_RELAYS: &[&str] = &["wss://relay.ngit.dev"];
```

Follow the livestream app's pattern (`STREAMING_RELAYS` in `notedeck_livestream/src/subscription.rs`).

---

## Known Pitfalls

| Pitfall | Mitigation |
|---------|------------|
| Kind 1111 has strict NIP-22 tag structure; reusing kind-1/NIP-10 reply flow breaks interop | Dedicated validator, never route through `post.rs` |
| Status events can be spoofed | Authority check: only item author or repo maintainer |
| 30617/30618 are addressable — without dedup, UI shows stale/duplicate repos | Dedup by `(kind, pubkey, d-tag)`, newest `created_at` wins |
| `t=root` (series root) vs `t=root-revision` (revision start) are distinct concepts | Dedicated `PatchSeries` domain model |
| `a`-tag queries need full NIP-33 coordinate `30617:<pubkey>:<repo-id>` | Explicit coordinate construction |
| `FilterBuilder::custom()` leaks memory | All validation in app code post-query |
| `ndb.search()` indexes content/profiles, not tags | Repo discovery via `kinds([30617])` + app-level tag matching |
| No code browsing — NIP-34 has no file tree kind | Default to Issues tab, not a "Code" tab |

## Components to Reuse

| Component | Location | Use |
|-----------|----------|-----|
| `AppContext` / `AppResponse` | `notedeck/src/app.rs` | Framework integration |
| `Router<R>` | `notedeck/src/route.rs` | Navigation |
| `RemoteApi` / `ScopedSubApi` | `notedeck/src/remote_api.rs` | Relay subscriptions |
| `JobPool` / `JobCache` | `notedeck/src/jobs/` | Async work |
| `ProfilePic` | `notedeck_ui/` | Avatars |
| `padding()`, `hline()` | `notedeck_ui/` | Layout |

**Cannot reuse as-is:**
- `NoteView` / `NoteContents` — hard-fails on non-kind-1
- `post.rs` — emits kind-1/NIP-10, not kind-1111/NIP-22

---

## CLI Tool (post-MVP, separate plan)

A `notedeck-git` CLI following `gh`/`glab` patterns and [clig.dev](https://clig.dev) guidelines. Shares the `notedeck_git` domain crate (parsing, validation, status resolution). Separate binary in `crates/notedeck_git_cli/`.

Command pattern: `notedeck-git <noun> <verb> [flags]`

### Command mapping: `gh` → `notedeck-git` → NIP-34

| `gh` command | `notedeck-git` equivalent | NIP-34 basis | Notes |
|-------------|--------------------------|-------------|-------|
| `gh repo list` | `repo list` | kind 30617 query | List repos from subscribed relays |
| `gh repo view` | `repo view <naddr>` | kind 30617 + 30618 | Show metadata, clone URLs, maintainers, refs |
| `gh repo clone` | `repo clone <naddr>` | `clone` tag on 30617 | Resolves clone URL from announcement, runs `git clone` |
| — | `repo add <naddr>` | kind 30617 | Subscribe to a repo (no `gh` equivalent — decentralized discovery) |
| `gh issue list` | `issue list` | kind 1621 + 1630-1633 | Filter by `--state open/closed` |
| `gh issue view` | `issue view <id>` | kind 1621 + 1111 | Show issue body (Markdown) + threaded comments |
| `gh issue create` | `issue create` | publish kind 1621 | `--title`, `--body`, `--label` flags (v2 — write path) |
| `gh issue close` | `issue close <id>` | publish kind 1632 | Publishes status event (v2) |
| `gh issue reopen` | `issue reopen <id>` | publish kind 1630 | Publishes status event (v2) |
| `gh issue comment` | `issue comment <id>` | publish kind 1111 | NIP-22 comment (v2) |
| `gh pr list` | `pr list` | kind 1618 + 1630-1633 | Filter by `--state open/applied/closed/draft` |
| `gh pr view` | `pr view <id>` | kind 1618/1619 + 1111 | Show PR description, tip, clone URL, comments |
| `gh pr checkout` | `pr checkout <id>` | `clone` + `c` tags on 1618 | `git fetch <url> refs/nostr/<event-id>` + checkout |
| `gh pr diff` | `pr diff <id>` | kind 1618 | Fetch ref and run `git diff` locally |
| `gh pr create` | `pr create` | publish kind 1618 | Push to `refs/nostr/<id>`, publish event (v2) |
| `gh pr close` | `pr close <id>` | publish kind 1632 | Publishes status event (v2) |
| `gh pr merge` | — | — | No server-side merge. Maintainer merges locally, publishes kind 1631 (Applied) |
| `gh pr review` | — | — | No approve/request-changes semantics in NIP-34. Use comments. |
| — | `patch list` | kind 1617 + 1630-1633 | No `gh` equivalent — email-style patches |
| — | `patch view <id>` | kind 1617 + 1111 | Show diff + comments |
| — | `patch apply <id>` | kind 1617 content | Pipe `git format-patch` content to `git am` |
| — | `patch send` | publish kind 1617 | `git format-patch` → publish (v2) |
| `gh search issues` | `search --type issue <query>` | client-side filter | No server-side search — filter locally by tag values |
| `gh search prs` | `search --type pr <query>` | client-side filter | Same |
| `gh status` | `status` | cross-repo query | Your authored/mentioned issues, patches, PRs across repos |
| `gh api` | `api <filter-json>` | raw nostr REQ | Send raw filters to relays, return events as JSON |
| `gh label list` | `label list` | `t` tags on 1621/1617/1618 | Read-only — labels are tags on events, not separate entities |
| `gh auth` | `auth login/logout/status` | nsec/bunker | Nostr key management |
| `gh browse` | `browse` | `web` tag on 30617 | Open repo's web URL in browser |

### What NIP-34 doesn't support (no `gh` equivalent possible)

| `gh` feature | Why it's absent | Workaround / future |
|-------------|----------------|---------------------|
| `gh pr merge` | No server-side merge. Git servers are "dumb data relays." | Maintainer merges locally, publishes kind 1631 (Applied) status |
| `gh pr review --approve` | No approval/request-changes workflow | Use comments (kind 1111). Could be a NIP extension. |
| `gh pr checks` / `gh run` | No CI/CD integration | Future: independent CI services via nostr webhooks (fiatjaf's vision) |
| `gh release` | No release kind in NIP-34 | Kind 30063 exists separately (not NIP-34) |
| `gh project` | No project boards | Not in scope for nostr git |
| `gh repo create` | Repos aren't "created" — they're announced | `repo init` publishes kind 30617 announcement for existing git repo |
| `gh repo fork` | No fork graph. Repos are independent. | Publish your own 30617 with `t=personal-fork` tag |
| `gh repo delete` | Addressable events can be updated but not truly deleted | Publish updated 30617 removing content, or stop announcing |
| `gh secret` / `gh variable` | No server-side config | N/A |
| `gh codespace` | No cloud dev environments | N/A |
| `gh wiki` | No wiki kind | N/A |
| Code browsing (`gh repo view --files`) | No file tree kind in NIP-34 | Use `clone` tag to clone locally |
| Assignees, milestones | No structured metadata beyond `t` tags | Could be NIP extension |
| PR branch protection rules | No server-enforced rules | Maintainer trust model via pubkey authority |

### Design principles

**Human-first ([clig.dev](https://clig.dev)):** Human-readable default output, `--help` with examples, TTY color detection, spinners for relay queries, errors rewritten for humans.

**Agent-first ([agent CLI design](https://justin.poehnelt.com/posts/rewrite-your-cli-for-ai-agents/)):** `--json` for both input and output (accept full structured payloads via `--params`), `schema` subcommand for machine-readable introspection (parameter types, required fields), `--dry-run` for mutations, NDJSON streaming for large result sets. This makes the CLI native to AI agent workflows — Daniel's second user story.

### Common flags (consistent across all commands, following `gh` conventions)

```
-R, --repo <naddr>       target a specific repo (default: from .git/config or prompt)
-r, --relay <url>        override relay (repeatable)
--json [<fields>]        structured JSON output (optionally specific fields)
-q, --jq <expr>          filter JSON output with jq
-s, --state <str>        open/applied/closed/draft/all (default: open)
-l, --label <str>        filter by label (t tag)
-L, --limit <n>          max results (default: 30)
-w, --web                open in browser (uses web tag from 30617)
--no-color               disable color (also respects NO_COLOR env)
--dry-run                validate without publishing (v2 write commands)
--params <json>          accept structured input (agent-friendly)
```
