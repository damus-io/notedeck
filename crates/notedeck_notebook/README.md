# Notebook

An Obsidian-style infinite canvas ([JSON Canvas](https://jsoncanvas.org/)) built
as a Notedeck application. A canvas — its nodes, edges, positions and text — is
stored as **nostr events** in the local nostrdb and reconstructed on the fly;
there is no separate mutable document.

> **Status: wired end-to-end.** The event schema, reducer and ndb fold live in
> `src/event.rs`; the action/persistence layer in `src/store.rs`; a live
> subscription-backed reducer (`CanvasSync`) and the `CanvasView → JsonCanvas`
> bridge (`src/convert.rs`) in `src/lib.rs`. The running app renders the
> nostr-backed canvas and turns drags/edits/creates into signed events. Like
> headway, this is **local-only** for now: events are signed and ingested into
> the local nostrdb but not yet published to relays.

## Overview

Notebook is **event-sourced**, exactly like [`headway`](../notedeck_headway): the canvas
you see is a pure function of a set of append-only events, not a thing you mutate
in place.

- A [`CanvasReducer`] folds all the events into a [`CanvasView`]
  (`event::reduce` / `event::load_canvas`). Reduction is deterministic and
  order-independent, so replaying the whole log always yields the same canvas.
- Every change is an **append**, never an in-place update, so there's no state
  to get out of sync with.

```
events (nostrdb)  ──Ndb::fold──▶  CanvasReducer  ──finalize──▶  CanvasView  ──▶  UI
   ▲                                                                            │
   └──────────────── sign + ingest ◀──────────────────────────── CanvasAction ◀┘
```

### How this differs from headway

Headway has one immutable anchor (the issue) with a few small overlays. A canvas
has a **larger, hotter mutable surface**: dragging and resizing fire constantly,
text is edited independently, and nodes have a z-order. So the overlay split is
finer, and two canvas-specific ideas are added:

1. **Geometry and content are separate overlays.** A move by one author and a
   text edit by another then merge cleanly — independent latest-wins, no lost
   update — the same reason headway keeps placement separate from subject.
2. **z-index reuses fractional ranking.** Node stacking order is a fractional
   `rank_between` rank (the same machinery headway uses for cards in a column),
   so restacking republishes one small event and never reindexes the canvas.
3. **Durable state vs. ephemeral presence.** Durable geometry is published only
   on *gesture-end* (drag/resize release). High-frequency in-flight positions and
   cursors are meant to be broadcast as **ephemeral** events (`KIND_PRESENCE`,
   kind 21606) that are **never folded into the document**. (Reserved; not yet
   implemented.)

## Event shapes

A canvas's addressable coordinate is `31606:<author-hex>:<canvas-id>`
(`event::canvas_address`).

| concept           | kind    | addressable | mutable                          |
| ----------------- | ------- | ----------- | -------------------------------- |
| **canvas**        | `31606` | yes — `d` = canvas id        | replaceable     |
| **node creation** | `1606`  | no — node id = event id      | immutable       |
| **node transform**| `31608` | yes — `d` = `canvas:node`    | latest-wins     |
| **node content**  | `31609` | yes — `d` = `canvas:node:c`  | latest-wins     |
| **edge**          | `31610` | yes — `d` = `canvas:edge`    | latest-wins     |

> **All five kinds are provisional** (cf. headway's own `30619`/`30620`
> caveat). These numbers are the most likely thing to change.

### Canvas — kind `31606` (addressable)

The document: a title, the membership list, and the visibility mode.

```jsonc
{
  "kind": 31606,
  "tags": [
    ["d", "<canvas-id>"],
    ["title", "Architecture sketch"],
    ["mode", "closed"],                  // "open" | "closed" (default closed)
    ["p", "<collaborator-hex>"]          // zero or more members
  ]
}
```

Membership doesn't appear in the canvas as nodes; nodes point *at* the canvas via
an `a` tag (like headway issues point at a board), so the canvas event stays
small regardless of size.

### Node creation — kind `1606` (immutable)

The node id is **this event's id** — a stable handle every overlay and edge
references. Carries the immutable `type` plus a creation snapshot of geometry and
content, used as the fallback until overlays supersede it.

```jsonc
{
  "kind": 1606,
  "content": "markdown text…",            // text body (text nodes); fallback content
  "tags": [
    ["a", "31606:<author>:<canvas-id>"],   // which canvas
    ["type", "text"],                       // text | file | link | group (immutable)
    ["x","-273"],["y","-180"],["w","252"],["h","80"],
    // type-specific content tags, as relevant:
    ["url", "https://…"], ["file","img.png"], ["subpath","#heading"],
    ["label","Group A"], ["background","bg.png"], ["bgstyle","cover"]
  ]
}
```

### Node transform — kind `31608` (addressable) — the hot path

A full geometry snapshot plus z-rank and optional color. Drag, resize, restack,
recolor and delete all republish just this one small event.

```jsonc
{
  "kind": 31608,
  "tags": [
    ["d", "<canvas-id>:<node-hex>"],
    ["a", "31606:<author>:<canvas-id>"], ["e", "<node-id>"],
    ["x","-273"],["y","-180"],["w","252"],["h","80"],
    ["z", "m"],                            // fractional z-rank (rank_between)
    ["color", "3"]                          // optional canvasColor
    // ["del","1"]  → reversible tombstone (removes the node from the canvas)
  ]
}
```

### Node content — kind `31609` (addressable)

The node's editable payload, separate from geometry so a move and an edit don't
clobber each other. Same content-bearing fields as the creation snapshot.

```jsonc
{
  "kind": 31609,
  "content": "## edited markdown",
  "tags": [
    ["d", "<canvas-id>:<node-hex>:c"],
    ["a", "31606:<author>:<canvas-id>"], ["e", "<node-id>"],
    ["url","…"]  // and/or file/subpath/label/background/bgstyle
  ]
}
```

### Edge — kind `31610` (addressable)

An edge has no user-authored payload beyond its decorations, so the whole edge is
one addressable event — no separate immutable creation anchor.

```jsonc
{
  "kind": 31610,
  "tags": [
    ["d", "<canvas-id>:<edge-id>"],
    ["a", "31606:<author>:<canvas-id>"],
    ["from", "<node-id>"], ["to", "<node-id>"],   // endpoints (e-style id tags)
    ["fromside","right"], ["toside","top"],
    ["fromend","none"], ["toend","arrow"],
    ["color","4"], ["label","calls"]
    // ["del","1"]  → tombstone
  ]
}
```

## Resolution

The reducer resolves each canvas with one rule:

- **Latest-surfaced-wins** — for every overlay (transform, content, edge), the
  newest *surfaced* event wins, ties broken deterministically by author bytes.
  Geometry comes from the latest transform (else the creation snapshot); content
  from the latest content overlay (else the creation snapshot); z-order sorts
  nodes back-to-front by rank.

### Authority is a visibility filter, not a validity gate

Unlike headway, where an unauthorised edit is simply dropped, **anyone may append
events to any canvas** — it's permissionless. What the canvas owner and listed
members control is only what's **surfaced**:

- An author is **surfaced** if the canvas is `open`, or they are the owner or a
  listed member (`p` tag).
- A node created by a non-surfaced author is collected onto
  [`CanvasView::pending`] instead of the main view — a proposal awaiting
  promotion (add them as a member, or flip the canvas to `open`).
- Overlays only count toward a node's resolved state if their author is
  surfaced. So a stranger can't move or retitle a member's node in the default
  view — but a member can edit anyone's, and the owner can edit everything.

This means a stranger can always *extend* a canvas (propose nodes/edits); they
just don't appear until surfaced. Flipping to `open` mode surfaces everyone at
once.

### Edge & deletion edge cases

- A transform (or edge) whose `del` is `"1"` is a **reversible tombstone**: the
  reducer drops the element. Republish a normal transform/edge to restore it
  (same latest-wins rules as any edit), rather than a NIP-09 deletion.
- An edge is drawn only when **both** endpoints resolve to a live, surfaced node.
  Delete a node and its dangling edges drop out automatically.

## Ranking (z-order)

Nodes are stacked by a **fractional rank** — lowercase `a`–`z` strings compared
lexicographically (`event::rank_between`, mirroring headway). Restacking mints a
rank strictly between two neighbours, so it republishes exactly one transform and
never reindexes the canvas. Nodes that have never been explicitly stacked sort by
creation time.

## Known limitations / future work

- **Single-author query.** `notebook_filter` fetches one author's events, so the
  reducer currently sees only the owner's contributions. The *reducer itself is
  already multi-author* (it keys overlays by author); surfacing collaborators
  needs an additional `#a`-tag filter on the canvas address. Same boundary
  headway started from.
- **Same-second ordering.** `created_at` is whole seconds; concurrent edits in
  the same second resolve by the author-bytes tiebreak. Publishing geometry only
  on gesture-end keeps this rare; a logical-clock tag is the general fix, and
  matters more here than in headway given canvas edit rates.
- **Provisional text content.** Text nodes carry their body as a single
  latest-wins string (kind `31609`), so two people editing the same node's text
  concurrently lose one edit. The node identity and the geometry/content split
  are stable, so this is an additive upgrade later (a CRDT content kind with a
  fallback chain), not a rewrite.
- **No relay sync** yet (local-only), and ephemeral presence (`KIND_PRESENCE`)
  is reserved but unimplemented.

## Source map

- `src/event.rs` — the pure schema: builders, parsers, the [`CanvasReducer`]
  (`finalize` / incremental `reduce_delta` / `fold_canvas` / `pick_canvas`),
  `rank_between`. No I/O.
- `src/store.rs` — the action/persistence layer: `CanvasAction` + `apply`, which
  sign and ingest the `event.rs` builders into nostrdb (mirrors `headway::store`,
  with the same `Publisher`/`NoPublish` seam). Egui-free.
- `src/lib.rs` — the `Notebook` Notedeck `App` plus `CanvasSync`, the live
  subscription-backed reducer that folds once then feeds in only freshly-arrived
  notes. Maps UI intents to `CanvasAction`s.
- `src/convert.rs` — `CanvasView → jsoncanvas::JsonCanvas` for rendering.
- `src/ui.rs` — the egui canvas renderer (draggable/selectable/editable nodes),
  reporting committed edits back as a `UiIntent`.

[`CanvasReducer`]: src/event.rs
[`CanvasView`]: src/event.rs
[`CanvasView::pending`]: src/event.rs
