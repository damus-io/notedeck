---
name: headway
description: Read and edit a Headway kanban board from the command line via the `headway` CLI (crates/headway_cli). Use when the user wants to view the board, add/move/edit/archive cards, or shuffle work between columns like Backlog, Todo, In Progress, In Review, and Done — e.g. "move X to done", "show the board", "add a card to todo".
---

# Headway board CLI

`headway` is a CLI over a running notedeck's embedded relay. It keeps its **own**
nostrdb cache, reconciles with the relay each run (NIP-77 negentropy, falling
back to NIP-01, or fully offline against the cache), folds the board locally, and
forwards edits back so the running app sees them. Source: `crates/headway_cli`.

## Running it

Prefer a built binary; fall back to cargo:

```bash
# build once, then call the binary directly (fast, no rebuild per command)
cargo build -p headway_cli            # produces target/debug/headway
target/debug/headway <command>

# or, one-off:
cargo run -q -p headway_cli -- <command>
```

In examples below, `headway` means whichever form you're using.

## Logging in

Everything operates on your own board once you're logged in — `show` to read,
the rest to edit. Relay defaults to `ws://127.0.0.1:6677` (notedeck's embedded
relay); override with `--relay <url>` or `HEADWAY_RELAY`. If no relay answers,
the CLI works offline against its cache and edits reach the app on the next
connected run.

If a command fails because you're not logged in, ask the user to run
`headway login`. Don't handle the key yourself.

## Multiple boards

A board is identified by a slug scoped to your key, so one identity can hold
several boards (e.g. a personal `headway` board and a `work` board).

**Always target a non-default board with the per-run `--board <id>` flag — never
the stateful `headway board <id>` switch.** Pass `--board <id>` on *every*
command in a sequence, so each call is self-contained:

```bash
headway --board work show
headway --board work add "Fix the relay reconnect" --col todo -l bug
headway --board work move 1a2b3c4d… --col done
```

Why avoid the stateful switch: `headway board <id>` persists the selection to a
file (`<data-dir>/headway-cli/board`) that is **shared mutable state**. The
running notedeck app — or any other `headway` process — can flip it between your
commands, so a `show` that targeted `work` can be followed by an edit that
silently lands on `headway` (you'll see a confusing "no card matching" when the
card "vanishes"). The `--board` flag is scoped to one run and can't be changed
underneath you, so a multi-step edit always hits the board you meant.

Board selection precedence, highest first: the `--board <id>` flag (one run
only) → the board named by a `headway:<board>/<word-id>` card ref (see below) →
`$HEADWAY_BOARD` → the board stored by `headway board <id>` → the default
`headway`. If you're acting on many boards in one session, prefer `--board`; only
fall back to `$HEADWAY_BOARD` (an env var, also stable for the session) when you
truly want every command to default to the same non-default board.

**Full card refs self-route.** A selector like `headway:commerce/purse-metal-toilet`
already names its board, so `headway show headway:commerce/purse-metal-toilet` (and
`move`, `comment`, …) targets `commerce` automatically — no `--board` needed. The
`headway:<board>/<word-id>` that `show` prints is a working address wherever you
paste it, which gives you the same self-contained, un-raceable targeting that
`--board` does — prefer either over relying on the stateful switch. The
scheme-less shorthand `commerce/purse-metal-toilet` self-routes too, and hex
prefixes resolve against the current board (so they still need `--board` to reach
another one). A **bare word-id** (`purse-metal-toilet`, no board segment) does
*not* self-route — it can't pick a board on its own — but it *does* resolve
against the board you've already selected, so `headway --board commerce show
purse-metal-toilet` works without repeating the slug in the id. Like a hex prefix,
a bare word-id also matches by **unique prefix** — `headway --board commerce show
purse-metal` (or even `purse`) resolves as long as it's unambiguous. Anything that
isn't purely hex digits is treated as a word-id, so only an all-`0-9a-f` selector
is a hex prefix. When a guess misses, the error suggests the real cards that share
its leading word instead of a bare "no card matching". Two refs naming different boards
in one command — or a ref that disagrees with an explicit `--board` — are an
error, not a silent resolution on the wrong board.

`headway board` with **no argument** is a harmless read — it lists the boards in
the cache and marks the current one with `*`; use it to discover slugs. Just
don't rely on its persisted `*` selection for edits. To create a board that
doesn't exist yet, `headway --board work seed`.

## The golden rule: `show` before you edit

Cards are addressed by their **event id**, and columns by **id or
case-insensitive name**. When scripting the CLI, pass a hex `id` from
`show --json`. Any unique prefix resolves, so the full 64-char id is overkill —
a **16-char (8-byte) prefix** is plenty for a board with a handful of cards, and
even an 8-char prefix is usually unambiguous. Use a short prefix for automated
edits; just lengthen it (or fall back to the full id) if you ever hit an
"ambiguous card prefix" error. The human-readable
`show` instead displays a muted **reference** like `headway:headway/maple-river-canyon`
(a friendly rendering of that same event id, for quoting in commits/chat); it also
resolves as a `<card>` argument, but prefer the hex id for automated edits.
Always run `show` first to read the current ids and column names, then act on what
you actually see — never assume an id or that a card is where you expect.

```bash
headway show            # human-readable: columns, titles, labels, word-ids
headway show --archived # also list archived cards in full (default: count only)
headway show --all      # every board in the cache, each printed in full and led
                        # by its slug (with --json, a JSON array of boards)
headway show --json     # machine-readable, for parsing (always includes archived)
headway show <card>...  # print the given cards (word-id or hex) in full
                        # `git show`-style detail, not the whole board; with
                        # --json each card gains a `column` field for the column
                        # it sits in
```

By default `show` collapses archived cards to a one-line count to keep the board
readable; pass `--archived` to list them (e.g. to find an id for `restore`).

`show` prints each card as `<title>  [labels]  headway:<board>/<word-id>`, with
the reference muted at the end of the line.

**Which form to use — MANDATORY when talking to a human** (chat, a commit
message, a PR, a board comment): refer to every card by its **full scheme
reference**, `headway:<board>/<word-id>` — the exact string `show` prints, scheme
*and* board included (`headway:dave/maple-river-canyon`; a notebook node is
`notebook:mango-sibling-false`). This is the **only** form the user's client
parses into a live link/chip. Any other form renders as dead text, so it is
never acceptable in human-facing prose.

Do **not** use, ever, when addressing a human:

- `headway#maple-river-canyon` or `dave#maple-river-canyon` — the `#`/hash form
  does **not** linkify. This is the most common mistake; there is no `#` in a
  Headway reference.
- a bare `maple-river-canyon` (no scheme, no board) — doesn't resolve at all.
- the scheme-less `dave/maple-river-canyon` — fine as a CLI argument, but it does
  **not** linkify in prose; always add the `headway:`/`notebook:` scheme when
  writing for a human.

The full `headway:<board>/<word-id>` is also self-routing and unambiguous no
matter which board is current when it's read. Apply this to **every** ref in a
message, not just the first — a comment that names five cards writes all five as
full scheme references. When *you* edit the board (move, label, archive, …), pass
the canonical **hex id** from `show --json` instead, so an automated edit can
never hit the wrong card.

All of these resolve as a `<card>` argument, to the same card every time:

- a hex event id, full or any unique prefix (a 16-char prefix is plenty) — preferred for editing
- `headway:dave/maple-river-canyon` — the full reference; it names its board, so
  it self-routes there without `--board` (see Multiple boards)
- `dave/maple-river-canyon` — the scheme-less shorthand; self-routes too. A bare
  `maple-river-canyon` with no board segment is **not** a card ref and won't resolve.

Default board columns: **Backlog**, **Todo**, **In Progress** (`in-progress`),
**In Review** (`in-review`), **Done** (`done`). A column argument matches an id
or a name case-insensitively, so `--col "in progress"`, `--col in-progress`, and
`--col "In Progress"` are equivalent.

## Commands

| Command | What it does |
| --- | --- |
| `show [cards...] [--archived] [--all] [--json]` | Print the board, or only the given cards (`--archived` lists archived cards; `--all` prints every board) |
| `seed` | Create the default board if none exists |
| `add <title...> [--col <c>] [-l <labels>] [--parent <card>] [--desc <text>\|--desc-file <path>]` | Add a card (defaults to the first column; `-l`/`--label` tags it; `--parent` creates it as a subissue; `--desc`/`--desc-file` sets its description at creation) |
| `move <card> --col <c> [--row <n>]` | Move a card to a column (optional position) |
| `title <card> <title...>` | Edit a card's title |
| `desc <card> <text...>` | Edit a card's description |
| `label <card> [labels...]` | Set labels — positional, comma-separated, or `-l` (no labels clears them) |
| `priority <card> <level>` | Set priority: `none`/`low`/`medium`/`high`/`urgent` (`none` clears it) |
| `parent <card> [parent]` | Make a card a subissue of `[parent]`; omit the parent to detach |
| `block <card> --on <other>` | Mark `<card>` as blocked by `<other>` (see Dependencies) |
| `unblock <card> --on <other>` | Remove the `<card>`-blocked-by-`<other>` edge |
| `relate <card> --to <other>` | Relate two cards (an undirected "see also"; see Dependencies) |
| `unrelate <card> --to <other>` | Remove the relation (from either endpoint) |
| `due <card> <date>` | Set a due date (`YYYY-MM-DD`, or `none` to clear) |
| `estimate <card> <n>` | Set an estimate — a number (or `none` to clear) |
| `seq <card> <pos> [--in <c>]` | Position a card in a container's work-order (see Work order) |
| `next [--in <c>] [--ready] [-n <k>]` | Print the ready frontier — what to work on next (see Work order) |
| `comment <card> <text...> [--reply-to <c>]` | Comment on a card (NIP-22); `--reply-to` threads under another comment |
| `delete <card>` | Remove a card (reversible tombstone) |
| `archive <card>` | Archive a card off the board |
| `restore <card>` | Restore an archived card |
| `link <card> --to <board>` | Also place the card on another board (it stays on this one) |
| `move-board <card> --to <board>` | Move the card off this board onto another |
| `board [id]` | Switch the current board to `id`, or (no arg) list boards and mark the current one |
| `rename <title...>` | Rename the current board's display title (slug unchanged) |
| `login <nsec>` | Store a signing key so later runs just work |
| `logout` | Forget the stored signing key |

`add` accepts `-l`/`--label` to tag the new card in one step. The flag is
repeatable and each value may be comma-separated, so `-l a,b --label c` and
`-l a -l b -l c` are equivalent:

```bash
headway add "Fix the relay reconnect" --col todo -l bug,p1
```

`label <card>` takes the same spellings — separate positionals, one
comma-separated positional, or the `-l`/`--label` flag — so these all set the
same two labels. Note it *replaces* the card's set rather than adding to it, and
`label <card>` with no labels at all is what clears:

```bash
headway label 1a2b3c4d… bug p1
headway label 1a2b3c4d… bug,p1
headway label 1a2b3c4d… -l bug,p1
headway label 1a2b3c4d…              # clears
```

`add` can also set the new card's description at creation, saving a follow-up
`headway desc <card> …` (and the need to learn the new card's id first).
`--desc <text>` takes the description inline; `--desc-file <path>` reads it from
a file, or from stdin when `<path>` is `-`, so a long multi-line markdown
description can heredoc in with no shell-escaping. The two are mutually exclusive
and trailing whitespace is trimmed (so a heredoc's closing newline doesn't ride
along). Both compose with `--col`/`-l`/`--parent`:

```bash
headway add "Rework the sync engine" --col todo --desc "Backfill stalls past the maxSyncEvents cap."
headway add "Migration plan" --col todo --desc-file - <<'EOF'
## Migration plan

1. Fork the session loop
2. Retire the per-app poller
EOF
```

Other flags: `--board <id>` (target another board for one run; see Multiple
boards), `--db <path>` (cache dir),
`--author <pk>` (read someone else's board), `-h`/`--help`. `--on <card>` names
the blocker for `block`/`unblock`; `--to` is the target board for
`link`/`move-board` and the partner card for `relate`/`unrelate`; `--in <c>` is
the container for `seq`/`next`.

When commenting a finished card's commit hash, a Dave agentic session should also
quote its own `agentium:` session ref (from `$AGENTIUM_SESSION`) beside the hash —
see AGENTS.md "When done with the work" and the `agentium` skill.

## Subissues

A card can be a **subissue** of one parent card (GitHub sub-issue semantics:
one parent per child, any number of children per parent). Use this instead of
the old hand-maintained epic pattern — an `epic` label plus a word-id checklist
in the description — whenever work breaks down into trackable pieces: the
rollup is derived from the board, so it can never go stale.

```bash
headway add "wire up the parser" --col todo --parent <epic>  # create as a subissue
headway parent <card> <epic>    # make an existing card a subissue (or re-parent)
headway parent <card>           # omit the parent to detach
```

Progress is **positional, not stored**: a child counts as done when it sits in
the last column of its board (Done on the default board), or is archived.
There is no checkbox to tick — moving the child card *is* the progress update.

How it renders:

- The board listing (`show`) marks parent cards with a dim `n/m` rollup.
- Card detail (`show <epic>`) gains a `subissue of` line on children and a
  derived checklist on parents:

  ```
  subissues (2/4 done)
      [x] route media loads through imgproxy   headway:headway/mushroom-include-wolf
      [ ] cap media cache size with eviction   headway:headway/extend-decrease-visit
  ```

- `show --json` gains `parent` (hex), `parent_ref` (a full
  `headway:<board>/<word-id>`), and `subissues` per card.

Notes: re-parenting that would create a cycle is refused; children may live on
a different board than the parent; nesting works (a child can itself be a
parent) but each rollup counts direct children only.

## Work order: what to do next (`next` and `seq`)

Beyond columns and subissues a board carries a **work-order** — a deliberate
sequence over cards that answers "what should I pick up next?". Two commands read
and write it:

- `headway next` prints the **ready frontier**: the cards workable *right now*,
  in work-order. `next` alone prints just the first (the single best next thing);
  `next --ready` prints the whole frontier; `next -n <k>` caps it at `k`. More
  than one card can be ready at once — that's the parallel-dispatch signal
  (independent cards you could hand to different workers at the same time).
- `headway seq <card> <pos>` positions a card in a container's work-order, which
  is how you *curate* the order `next` reads. `<pos>` is `--first`, `--last`,
  `--after <card>`, or `--before <card>`.

`--in <c>` names the container whose order you mean: a **card ref** targets that
card's subissues, a **board slug** targets the board root (its top-level cards);
omitted, it's the board root. `next` refuses to guess the board — pass
`--board <id>` or an `--in <headway:board/word-id>` ref (never the persisted
current board).

```bash
headway --board dave next               # the single best next card
headway --board dave next --ready       # the whole ready frontier, in order
headway --board dave next -n 3          # the top 3
headway seq <card> --first --in dave    # make <card> the first board-root task
headway seq <epic-child> --after <sib> --in <epic>   # order within an epic
```

**What "ready" means.** A card is ready when it is *not done* (not sitting in the
board's last column), *not blocked* (no `block` edge pointing at an unfinished
card — see Dependencies), and *not a parent with unfinished subissues* (an epic's
real work is its children, which are in the frontier themselves, so the epic card
isn't dispatchable). Note a card in **In Review** still counts as ready/workable —
only the last column (Done) reads as done — so `next` will resurface review-stage
cards.

**Ordering, and the priority caveat.** Within a container, members run in
`seq`-order where a `seq` has been set, else creation order (the board root falls
back to spatial column order). `next` does **not** sort by the `priority` field —
priority is a human-facing label, not an input to the frontier. So a board where
nobody has run `seq` has no real work-order: `next --ready` is just the board in
default order, and you should judge the biggest win yourself rather than trust the
first line. Curate with `seq` to make `next` meaningful.

## Dependencies: `block` / `unblock` (and `relate`)

A card can be **blocked by** any number of other cards — a directed dependency
edge saying "don't start this until that is finished". It is a separate axis
from parent/subissue: a card can be both a subissue and blocked, and an edge may
point at a card on another board.

```bash
headway block <card> --on <blocker>      # <card> is blocked by <blocker>
headway block <card> <blocker>           # same — the blocker may be a 2nd positional
headway unblock <card> --on <blocker>    # drop the edge
```

Unlike `parent` (one parent, re-parenting replaces it), blockers **accumulate**:
each `block` adds to the card's set, and `unblock` removes one edge.

An edge edit that changes nothing says so, and never as a resolution failure —
so don't retry with a longer id. Re-adding an edge that's already there (`block`,
`relate`) or dropping one that isn't (`unblock`, `unrelate`) is idempotent
success: exit 0 with `ok (0 events) — <card> is already blocked on <blocker>`
(`--json` adds a `"noop"` field). A `block` or `parent` that would close a cycle
is a real error, and names what it refused:

```
error: refused: blocking <card> on <blocker> would create a dependency cycle (…)
```

`error: action produced no events (unknown card or column?)` remains only for
edits the reducer genuinely couldn't resolve.

A blocker is **cleared** the same positional way a subissue is done — when it
sits in the last column of its board (Done), or is archived. There's nothing to
tick: moving the blocker *is* the unblock.

How it renders:

- The board listing (`show`) prefixes a blocked card with a dim `⊘` glyph. A card
  whose blockers are all cleared loses the glyph, so `⊘` always means "held back
  right now".
- Card detail (`show <card>`) gains `blocked by` / `blocks` sections — the
  forward edges and the reverse ones (cards this one is holding up) — with `x`
  marking a cleared blocker so open ones stand out:

  ```
  blocked by (2)
      [x] land the negentropy reconcile   headway:headway/mushroom-include-wolf
      [ ] cap media cache size            headway:headway/extend-decrease-visit
  ```

- `show --json` gains `blocked` (bool — any unfinished blocker), plus
  `blocked_by` and `blocks` arrays of `{id, ref, title, done}`.
- **`next` and `next --ready` skip a card with an unfinished blocker** — this is
  the main reason to record dependencies, since it keeps the frontier to work
  that's actually startable (see Work order).

`relate <card> --to <other>` is the third edge kind: an **undirected "see also"**
between two cards. It's symmetric (both ends list each other, and `unrelate`
works from either), may cross boards, and is purely informational — it carries no
ordering and never affects `is_blocked`, the ready frontier, or any rollup. Card
detail shows it as a `related` section and `show --json` as a `related` array.

## Cross-board cards: `link` and `move-board`

Board membership is **placement-driven**: the same card — one issue with all
its overlays (title, description, labels, comments, parent/subissues) — can sit
on several boards at once. Two commands manage this:

```bash
headway --board work link 1a2b3c4d… --to personal        # now on both boards
headway --board work move-board 1a2b3c4d… --to personal  # re-homed: off work, on personal
```

- `link` adds a placement on the target board and keeps every placement the
  card already has. Edits made anywhere show everywhere — it's the same card,
  not a copy. Re-linking an already-linked card is harmless (it just re-ranks).
- `move-board` is link + remove from the source board: the card keeps its id,
  word-id, and all overlays, and now lives only on the target (plus any other
  boards it was already linked to).
- On the target, the card lands in the column whose **id matches its current
  column** (e.g. a card in `in-review` stays in `in-review`), falling back to
  the target's first column when no such column exists there.
- The card is resolved on the **source** board, so combine `--board <source>`
  with `--to <target>`. The target board must already exist — seed it first
  with `headway --board <target> seed` if it doesn't.

## Typical workflow

Move a card from In Progress to Done:

```bash
headway show --json                  # match the title, grab its hex `id`
headway move 1a2b3c4d… --col done    # move by hex id (a column may match by name)
headway show                         # verify it landed in Done
```

To address a card by title, read `show --json` and match the title to its hex
`id`, then pass that id. Resolution errors are explicit: an ambiguous hex prefix
says "ambiguous card prefix", an unknown reference "no card matching", and a bad
column lists the valid column names — re-read `show` and retry with a corrected
argument.

## Notes

- Edits print `ok (N events)`; offline edits append `— offline, not forwarded to
  the app`, meaning they're cached but haven't reached the running notedeck yet.
- `seed` errors if a board already exists; that's expected — just `show` instead.
- The cache lives at `<data-dir>/headway-cli` unless `--db` overrides it. The CLI
  and the running app converge through the relay, so either side's edits show up
  on the other after a reconcile.
