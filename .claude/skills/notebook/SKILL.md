---
name: notebook
description: Read and edit a notebook canvas from the command line via `nd notebook` (the notebook tools of the running notedeck, crates/notedeck_notebook). Use when the user wants to view the vault or a canvas, add/move/edit/recolor/delete nodes, connect or disconnect edges, restack, seed, or rename a canvas — e.g. "show the notebook", "add a node", "move that node", "connect these two nodes".
---

# Notebook canvases from the command line: `nd notebook`

`nd notebook <command>` runs one of the running notedeck's notebook tools over
its local socket (see the `nd` skill). The notedeck folds the vault — its
canvases and longform notes — from its own nostrdb with the same reducer the
egui app uses, and signs edits as its selected account, so the app and every
`nd` call see the same canvas with nothing to sync.

## Running it

```bash
nd notebook show                          # nd on PATH (scripts/install-clis)
cargo run -q -p nd_cli -- notebook show   # or from a checkout
```

`nd notebook --help` lists the commands; `nd notebook <command> --help` prints
that command's arguments and the tool it runs. Every command takes `--json`,
`--pager`/`--no-pager` and `--color <auto|always|never>`.

There is no login and no `--author`/`--relay`/`--db`/`--nsec`: the notedeck
answering the socket is the identity. `notebook <command>` still works through
a shim (`scripts/shims/nd-shim`) that runs `nd notebook <command>`, but type
`nd notebook` in anything new.

## Which canvas

A notebook holds many canvases. `nd notebook show` with no argument lists the
**vault**: every canvas and longform note, newest-edited first, each with its
`notebook:<word-id>` ref. Edits take `--canvas <canvas>` — a
`notebook:<word-id>` ref, a `nostr:naddr`, or its `d` (a unique prefix will
do) — and infer it only when there is a single canvas, so name it whenever
there are several.

## The golden rule: `show` before you edit

Each node has a friendly **reference** — the `notebook:` scheme then three BIP-39
words encoding the leading bits of its event id, e.g.
`notebook:maple-river-canyon`. It's a sayable rendering of the node's real
identity (its 64-char nostr event id), the notebook sibling of headway's
`headway:<board>/…` cards — single-segment (no canvas), because a node's identity
is its event id, not the canvas it happens to sit on. A canvas and a longform
note have refs of the same form. Quote them in chat or commits, and pass them
back to any command that takes a `<node>` or `--canvas`.

A `<node>` argument resolves as the full 64-char **hex id**, any unique **hex
prefix**, or a **`notebook:<word-id>` reference**. Edges are addressed by their
**edge id** as `show` lists it, or a unique prefix. Always run `show` first to
read the current ids, then act on what you actually see — never assume an id or
that a node is where you expect.

```bash
nd notebook show                           # the vault: canvases and notes, with refs
nd notebook show notebook:gloom-shove-melody   # one canvas: title, nodes, edges, pending
nd notebook show <ref>... --json           # canvas {id, title, nodes, edges, pending, …},
                                           # note {d, ref, title, content, …} or node {id, ref, x, y, w, h, text, …}
nd notebook vault [<note>...]              # the longform notes, with their markdown content
```

A canvas prints its title, then each node as `<first line of text>  (x,y w×h)
notebook:<word-id>`, then its edges and any **pending** nodes (proposals on a
closed canvas). One ref answers its object; several answer an array.

**Which form to use:** refer to a node or canvas by its **`notebook:<word-id>`
reference** when talking to a human (it's sayable and renders as a live inline
chip in notes and chat). When *you* script an edit, the hex id from `show
--json` can never be ambiguous; the reference resolves too and is fine for
one-offs.

## Commands

Every command below is `nd notebook <command>`; the edits also take `--canvas`.

| Command | What it does |
| --- | --- |
| `show [refs...] [--json]` | List the vault, or show the given canvases, notes or nodes |
| `vault [notes...]` | List the longform notes, or answer the given ones with their content |
| `seed [title...] [--canvas <d>]` | Create a new, empty canvas (default title `Notebook`); `--json` answers its `canvas` d and `ref` |
| `add <text...> [-x -y -w --height]` | Add a text node, optionally placed/sized; `--json` answers its `ref` |
| `move <node> [-x -y -w --height]` | Move/resize a node (unset fields keep current geometry) |
| `edit <node> <text...>` | Replace a node's text |
| `color <node> [color]` | Recolor a node: a preset `1`–`6` or `#rrggbb` (`none`/`-`/omitted clears) |
| `restack <node> <index>` | Restack a node to a display index (`0` = bottom) |
| `delete <node>` | Remove a node (reversible tombstone) |
| `connect <from> <to> [--from-side <s>] [--to-side <s>]` | Draw an edge between two nodes |
| `disconnect <edge>` | Remove an edge (id from `show`) |
| `rename <title...>` | Rename the canvas |

Re-drawing the same `connect <from> <to>` updates that edge rather than
stacking a duplicate (edge ids are stable per ordered pair, latest-wins).

### Geometry flags

`add` and `move` take geometry in canvas pixels: `-x`/`--x`, `-y`/`--y` for
position and `-w`/`--w`, `--height` for size (height has no `-h` short — that's
`--help`). `add` fills any unset field from defaults (a new text node is
`250×120` at the origin); `move` fills unset fields from the node's current
geometry, so `move <node> -x 400` slides it horizontally without touching its
size or `y`.

```bash
nd notebook add "ship the thing" -x 100 -y 200 --canvas notebook:gloom-shove-melody
nd notebook move 1a2b3c4d -x 400 -y 50 --canvas notebook:gloom-shove-melody
```

## Typical workflow

Add a node, then connect it to an existing one:

```bash
c=notebook:gloom-shove-melody
nd notebook show $c                                    # read the canvas + word-ids
nd notebook add "new idea" -x 300 -y 100 --canvas $c   # create the node
nd notebook show $c                                    # read back both word-ids
nd notebook connect notebook:maple-river-canyon notebook:glide-amber-mesa --canvas $c
nd notebook show $c                                    # verify the edge landed
```

## Notes

- Edits print `ok (N events)`.
- If a selector doesn't resolve, the error says so (ambiguous or unknown) —
  re-read `show` and retry with a longer prefix, the `notebook:<word-id>`
  reference, or the full id.
- A tool error goes to stderr with exit 1; no notedeck reachable is exit 2 (see
  the `nd` skill).
