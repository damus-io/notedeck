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

## The golden rule: `show` before you edit

Cards are addressed by a **short id prefix** (or full 64-char hex id), and
columns by **id or case-insensitive name**. Always run `show` first to read the
current prefixes and column names, then act on what you actually see — never
assume an id or that a card is where you expect.

```bash
headway show            # human-readable: columns, card prefixes, labels
headway show --archived # also list archived cards in full (default: count only)
headway show --json     # machine-readable, for parsing (always includes archived)
headway show <card>...  # print only the given cards (id or prefix), not the
                        # whole board; with --json each card gains a `column`
                        # field for the column it sits in
```

By default `show` collapses archived cards to a one-line count to keep the board
readable; pass `--archived` to list them (e.g. to find a prefix for `restore`).

`show` prints each card as `<8-char-prefix>  <title>  [labels]`. Use that prefix
(or any unique longer prefix) as the `<card>` argument.

Default board columns: **Backlog**, **Todo**, **In Progress** (`in-progress`),
**In Review** (`in-review`), **Done** (`done`). A column argument matches an id
or a name case-insensitively, so `--col "in progress"`, `--col in-progress`, and
`--col "In Progress"` are equivalent.

## Commands

| Command | What it does |
| --- | --- |
| `show [cards...] [--archived] [--json]` | Print the board, or only the given cards (`--archived` lists archived cards) |
| `seed` | Create the default board if none exists |
| `add <title...> [--col <c>] [-l <labels>]` | Add a card (defaults to the first column; `-l`/`--label` tags it) |
| `move <card> --col <c> [--row <n>]` | Move a card to a column (optional position) |
| `title <card> <title...>` | Edit a card's title |
| `desc <card> <text...>` | Edit a card's description |
| `label <card> [labels...]` | Set labels (no labels clears them) |
| `delete <card>` | Remove a card (reversible tombstone) |
| `archive <card>` | Archive a card off the board |
| `restore <card>` | Restore an archived card |
| `login <nsec>` | Store a signing key so later runs just work |
| `logout` | Forget the stored signing key |

`add` accepts `-l`/`--label` to tag the new card in one step. The flag is
repeatable and each value may be comma-separated, so `-l a,b --label c` and
`-l a -l b -l c` are equivalent:

```bash
headway add "Fix the relay reconnect" --col todo -l bug,p1
```

Other flags: `--board <id>` (non-default board), `--db <path>` (cache dir),
`--author <pk>` (read someone else's board), `-h`/`--help`.

## Typical workflow

Move a card from In Progress to Done:

```bash
headway show                         # find the card's prefix, confirm columns
headway move 1a2b3c4d --col done     # move it (a card or column may match by name)
headway show                         # verify it landed in Done
```

To address a card by title, read `show --json` and match the title to its id,
then pass a unique prefix of that id. Resolution errors are explicit: an
ambiguous prefix says "ambiguous card prefix", an unknown one "no card matching",
and a bad column lists the valid column names — re-read `show` and retry with a
corrected argument.

## Notes

- Edits print `ok (N events)`; offline edits append `— offline, not forwarded to
  the app`, meaning they're cached but haven't reached the running notedeck yet.
- `seed` errors if a board already exists; that's expected — just `show` instead.
- The cache lives at `<data-dir>/headway-cli` unless `--db` overrides it. The CLI
  and the running app converge through the relay, so either side's edits show up
  on the other after a reconcile.
