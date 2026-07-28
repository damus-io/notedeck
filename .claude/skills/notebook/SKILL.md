---
name: notebook
description: Read and edit a notebook canvas from the command line via the `notebook` CLI (crates/notebook_cli). Use when the user wants to view the canvas, add/move/edit/recolor/delete nodes, connect or disconnect edges, restack, seed, or rename the canvas — e.g. "show the notebook", "add a node", "move that node", "connect these two nodes".
---

# Notebook canvas CLI

`notebook` is a CLI over a running notedeck's embedded relay. It keeps its
**own** nostrdb cache, reconciles with the relay each run (NIP-77 negentropy,
falling back to NIP-01, or fully offline against the cache), folds the canvas
locally with the same reducer the egui app uses, and forwards edits back so the
running app sees them. Source: `crates/notebook_cli`; the cache/sync/relay/key
plumbing is shared with `headway` via `crates/relay_sync`.

## Running it

Prefer a built binary; fall back to cargo:

```bash
# build once, then call the binary directly (fast, no rebuild per command)
cargo build -p notebook_cli           # produces target/debug/notebook
target/debug/notebook <command>

# or, one-off:
cargo run -q -p notebook_cli -- <command>
```

In examples below, `notebook` means whichever form you're using.

## Logging in

The CLI reads and writes **your own** canvas once you're logged in. The author
whose canvas is loaded comes from `--author <pk>`, else the signing key's own
pubkey — so to read or edit without `--author` you need a key configured. Relay
defaults to `ws://127.0.0.1:6677` (notedeck's embedded relay); override with
`--relay <url>` or `$NOTEBOOK_RELAY`. If no relay answers, the CLI works offline
against its cache and edits reach the app on the next connected run.

If a command fails because no key is configured (`need --nsec to sign, or
--author to read a canvas`), ask the user to run `notebook login`. Don't handle
the key yourself.

## The golden rule: `show` before you edit

Each node has a friendly **word-id** — the canvas id, an `@`, then three BIP-39
words encoding the leading bits of its event id, e.g.
`notebook@maple-river-canyon`. It's a sayable rendering of the node's real
identity (its 64-char nostr event id), the notebook sibling of headway's
`headway#…` cards — distinguished by the `@` sigil so the two never get confused.
`show` prints it muted at the end of each node line; quote it in chat or commits,
and pass it back to any command that takes a `<node>`.

A `<node>` argument resolves, in order, as:

- the full 64-char **hex id**, or any unique **hex prefix** — canonical, best for
  scripted edits where you've pulled the id from `show --json`
- a **word-id**: `notebook@maple-river-canyon`, the bare `@maple-river-canyon`,
  or just `maple-river-canyon` — all resolve to the same node

Edges are addressed by their **edge id** (`<from-hex>-<to-hex>`, shown muted by
`show`) or a unique prefix. Always run `show` first to read the current ids, then
act on what you actually see — never assume an id or that a node is where you
expect.

```bash
notebook show              # human-readable: title, nodes, edges, pending
notebook show --json       # machine-readable, for parsing (each node has `id` + `word_id`)
notebook show <node>...    # print only the given nodes (hex id, word-id, or prefix)
notebook show <node> --json
```

`show` prints the canvas title (with `[open]` if open), then each node as
`<first line of text>  (x,y w×h)  <canvas>@<word-id>` with the geometry and
word-id muted, then edges as `<from-word-id> → <to-word-id>  <edge-id>`, then any
**pending** nodes (proposals on a closed canvas). If there's no canvas yet it
tells you to run `notebook seed`; with `--json` it prints `null`.

If a selector doesn't resolve, errors are explicit: an ambiguous prefix says
`ambiguous node prefix` / `ambiguous edge prefix`, an unknown one `no node
matching` / `no edge matching` — re-read `show` and retry with a longer prefix,
the word-id, or the full id.

**Which form to use:** refer to a node by its **word-id** when talking to a human
(it's sayable and they'll recognise it). When *you* script an edit, the hex id
from `show --json` is canonical and can never be ambiguous; the word-id resolves
too and is fine for one-offs.

## Commands

| Command | What it does |
| --- | --- |
| `show [nodes...] [--json]` | Print the canvas, or only the given nodes |
| `seed [title...]` | Create the canvas if none exists (default title `Notebook`) |
| `add <text...> [-x -y -w -h]` | Add a text node, optionally placed/sized |
| `move <node> [-x -y -w -h]` | Move/resize a node (unset fields keep current geometry) |
| `edit <node> <text...>` | Replace a node's text |
| `color <node> [color]` | Recolor a node (`none`/`-`/empty clears; `--color` also works) |
| `restack <node> <index>` | Restack a node to a display index (`0` = bottom) |
| `delete <node>` | Remove a node (reversible tombstone) |
| `connect <from> <to> [--from-side <s>] [--to-side <s>]` | Draw an edge between two nodes |
| `disconnect <edge>` | Remove an edge (id from `show`) |
| `rename <title...>` | Rename the canvas |
| `login <nsec>` | Store a signing key so later runs just work |
| `logout` | Forget the stored signing key |

`show`, `seed`, `login`, and `logout` aside, every command edits the canvas and
needs a signing key. Re-drawing the same `connect <from> <to>` updates that edge
rather than stacking a duplicate (edge ids are stable per ordered pair,
latest-wins).

### Geometry flags

`add` and `move` take geometry in canvas pixels: `-x`/`--x`, `-y`/`--y` for
position and `-w`/`--w`, `-h`/`--height` for size. `add` fills any unset field
from defaults (a new text node is `250×120` at the origin); `move` fills unset
fields from the node's current geometry, so `move <node> -x 400` slides it
horizontally without touching its size or `y`.

```bash
notebook add "ship the thing" -x 100 -y 200    # new node at (100,200)
notebook move 1a2b3c4d -x 400 -y 50            # reposition, keep size
```

Other flags: `--author <pk>` (read someone else's canvas), `--canvas <id>`
(non-default canvas, default `notebook`), `--db <path>` (cache dir), `--nsec
<nsec>` (key for this run; `$NOTEBOOK_NSEC` takes precedence over the stored
key), `-h`/`--help`.

## Typical workflow

Add a node, then connect it to an existing one:

```bash
notebook show                                          # read the canvas + word-ids
notebook add "new idea" -x 300 -y 100                  # create the node
notebook show                                          # read back both word-ids
notebook connect maple-river-canyon glide-amber-mesa   # draw an edge between them
notebook show                                          # verify the edge landed
```

For scripted edits where ambiguity must be impossible, pull the hex `id` from
`notebook show --json` and pass that instead of the word-id.

## Notes

- Edits print `ok (N events)`; offline edits append `— offline, not forwarded to
  the app`, meaning they're cached but haven't reached the running notedeck yet.
  `seed` prints `seeded canvas '<id>' (N events)`.
- `seed` errors if a canvas already exists; that's expected — just `show`
  instead. An edit that resolves to no change prints `action produced no events
  (unknown node or edge?)`.
- The cache lives at `<data-dir>/notebook-cli` unless `--db` overrides it. The
  CLI and the running app converge through the relay, so either side's edits show
  up on the other after a reconcile.
</content>
</invoke>
