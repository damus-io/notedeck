# autowork: one cross-app Headway deep link should cost one nav entry, not two

jb55: clicking an inline headway ref from another app needs **two** back presses, and
the first lands on the Headway board root instead of returning to the source app.
Root cause and the full design are on the container card and in the plan file.

    container: headway:notedeck/obey-margin-scale
    check:     cargo test -p notedeck -p notedeck_chrome --lib && cargo test -p notedeck_headway --lib
               (sum of the `test result: ok. N passed` lines; ~40s cold, stable)
    stop:      all five subissues are in In Review or Done, or the frontier is empty
    cap:       10
    plan:      /home/jb55/.claude/plans/human-nav-seems-kinda-dynamic-fiddle.md

Each subissue card's **description is the spec** — file paths, line numbers, the exact
shape to write, and its own tests. Read it before touching code; it is the only place
the investigation survives.

## The check is a drift column, not the gate

The gate is what CLAUDE.md already requires: **`./scripts/ci-local` before every
commit** (changelog trailers, fmt, clippy, tests, android). The check above is only a
cheap number for the ledger.

It deliberately omits `notedeck_headway`'s *integration* suite
(`tests/snapshot_tests.rs`), which is where commits 2 and 4 add their tests — that
suite is known-flaky (SIGILL: `headway:notedeck/man-eight-damp`; ingest-timeout races:
`headway:notedeck/staff-latin-like`), so a wobble there would drown the drift signal.
Run it explicitly for the cards that touch it:
`cargo test -p notedeck_headway --test snapshot_tests`.

Also run `cargo test -p notedeck_columns --test frame_alloc` on any commit that could
touch a per-frame path — its budget is bit-exact and fails loudly for a non-obvious
reason.

## Work order

Sequenced, with real blockers, so `headway next` is correct on its own:

1. `enable-floor-decrease` — notedeck: the `App::open_note_route` hook (defaulted, inert)
2. `super-hello-emotion` — headway: don't back out of a card that hasn't folded in
3. `cycle-crumble-jeans` — headway: factor the board switch out of `process_pending_open`
4. `wife-lion-address` — headway: implement the hook *(blocked on 1 + 3)*
5. `duck-echo-chronic` — chrome: push one routed entry *(blocked on 4 + 2)* — **the user-visible fix**

Commits 2 and 3 are genuinely independent of 1; the `seq` order is the review order.

## State at initiation (2026-09-23)

- Branch `headway`, HEAD `5f371ddc1063`. Investigation is **done** — no session needs to
  redo it. Nothing is implemented yet; the tree is clean apart from jb55's untracked
  `crates/headway_cli/examples/*.rs` and stray `data.mdb`/`lock.mdb`. **Leave those
  alone** — jb55 codes concurrently, so re-check the live tree before editing and never
  run `cargo fmt` or file writes over his uncommitted WIP.
- Board claims checked: the parent epic `headway:notedeck/soul-unlock-skate` is accurate
  and still open at 9/13. Its subissue `headway:notedeck/spell-unaware-educate` is
  **not** stale but **is** a partial red herring — the container card's comment records
  why its specced `handle_note_action(..) -> bool` shape cannot serve a cross-app open,
  and why `open_note_route` deviates. Keep both `spell-unaware-educate` and
  `faith-belt-bitter` open; this container does not close them.
- Seven further nav defects were found while tracing and are **deliberately out of
  scope** (listed at the end of the plan file). Do not fold them in. If one blocks a
  card, file it and `headway block` — otherwise leave it for jb55.

| # | date | card | before | after | what happened |
|---|------|------|--------|-------|---------------|
| 0 | 09-23 | *(initiation)* | 419 | 419 | investigated, seeded the five cards, wrote this ledger |
| 1 | 09-23 | `enable-floor-decrease` | 419 | 419 | added the defaulted `App::open_note_route` hook + the `NotedeckApp` fan-out; `802f471a860f` |

## `headway next` cannot advance this container on its own — read this first

Discovered in iteration 1. **No column on the `notedeck` board is marked
`terminal`** — not even Done (`headway show --board notedeck --json` reports
`"terminal": false` for all five). Two consequences, both of which will strand a
session that trusts `next` blindly:

1. A card moved to In Review (which CLAUDE.md requires, and which is where every
   card in this container will sit until jb55 verifies it) **keeps appearing in
   `headway next`**. Don't re-do it.
2. Blocker edges never clear, so `wife-lion-address` (4) and `duck-echo-chronic`
   (5) will **never** surface in `next`, however many blockers are finished.
   Verified: `enable-floor-decrease` is In Review and still reads `[ ]` under
   `wife-lion-address`'s *blocked by*.

So use the **Work order** list above as the real queue: take the lowest-numbered
card that is still in Backlog/Todo/In Progress, and treat a blocker sitting in In
Review or Done as satisfied. Use `next` only to confirm nothing was re-ordered.

Fixing this properly means `headway terminal in-review on` (or `done on`) on the
`notedeck` board, which changes `next` for **every** card jb55 has there — a
board-wide config call that is his, not a session's. Left alone deliberately.
