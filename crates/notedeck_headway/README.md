# Headway

A Linear/Trello-style issue & kanban tracker built as a Notedeck application.
Boards, columns and cards are stored as **nostr events** in the local nostrdb
and reconstructed on the fly — there is no separate database or mutable record.

> **Status: local-only.** Events are signed and ingested into the local nostrdb
> but are **not** published to relays yet. `src/store.rs` is the single place a
> `publish_note` call will be added when we go remote. Multi-author boards
> already work by construction (see [Resolution](#resolution)); they just aren't
> pointed at a relay.

## Overview

Headway is **event-sourced**: the board you see is a pure function of a set of
events, not a thing you mutate in place.

- A **card is immutable** — it's a nostr issue that's never rewritten.
- Everything mutable about a card (its title, description, labels, and which
  column it sits in) lives in *separate overlay events* keyed by the card's id.
- A reducer folds all the events into a `BoardView` (`event::reduce` /
  `event::load_board`). Reduction is deterministic and order-independent, so
  replaying the whole log always yields the same board.

This is why editing is robust: every change is an **append**, never an in-place
update, so there is no state to get out of sync with.

```
events (nostrdb)  ──Ndb::fold──▶  BoardReducer  ──finalize──▶  BoardView  ──▶  UI
   ▲                                                                            │
   └──────────────── sign + ingest (store::apply) ◀────────── BoardAction ◀────┘
```

## Specs utilized

Headway deliberately reuses the **NIP-34 issue** model and the
**ngitstack/gitworkshop** metadata extensions for everything a card needs, and
adds a thin custom **kanban layer** (board + placement) on top.

| Kind      | Role                       | Spec                              | Addressable           |
| --------- | -------------------------- | --------------------------------- | --------------------- |
| **1621**  | Card                       | [NIP-34] issue                    | no (immutable)        |
| **1985**  | Title edit **and** labels  | [NIP-32] label                    | no                    |
| **1624**  | Description edit           | gitworkshop *cover note*          | no                    |
| **30619** | Board                      | **custom** (provisional)          | yes — `d` = board id  |
| **30620** | Card placement             | **custom** (provisional)          | yes — `d` = board:issue |

### Provenance & references

- **NIP-34** — *Git stuff*: a card is a real kind-`1621` issue.
  <https://github.com/nostr-protocol/nips/blob/master/34.md>
- **NIP-32** — *Labeling*: title edits and labels are kind-`1985` label events,
  distinguished by their `L` namespace.
  <https://github.com/nostr-protocol/nips/blob/master/32.md>
- **gitworkshop / ngitstack — "Shared Issue / Patch / PR Metadata"**: the
  conventions for *post-creation labels*, *editable issue subjects*, and
  *cover notes* (editable, versioned descriptions). Headway follows these so the
  card layer stays compatible with git-over-nostr tooling.
  <https://gitworkshop.dev/danconwaydev.com/gitnostr.com/gitworkshop/tree/main/NIP.md#shared-issue-patch-pr-metadata>

> **Custom kinds `30619` / `30620` are provisional.** A card anchors (`a` tag)
> to a custom *board* kind rather than a NIP-34 repository (`30617`) — a
> deliberate "lightweight board, not a repo" choice. Boards are nostr-native and
> issue-compatible, but a generic NIP-34 git client won't render one as a repo.
> These two kind numbers are the most likely to change.

## Event shapes

A board's addressable coordinate is `30619:<author-hex>:<board-id>`
(`event::board_address`). The single default board uses board id `"headway"`.

### Board — kind `30619` (addressable)

```jsonc
{
  "kind": 30619,
  "tags": [
    ["d", "headway"],                 // board id
    ["title", "Headway"],
    ["description", "..."],           // optional
    ["col", "backlog", "Backlog"],    // ordered columns: ["col", <id>, <name>]
    ["col", "todo", "Todo"],
    ["col", "in-progress", "In Progress"],
    ["col", "done", "Done"]
  ]
}
```

The column *order* is the order of the `col` tags. Adding / renaming /
reordering / removing a column republishes the board event with an edited `col`
list (latest board event wins).

### Card — kind `1621` (NIP-34 issue, immutable)

```jsonc
{
  "kind": 1621,
  "content": "free-form body / description",
  "tags": [
    ["a", "30619:<author-hex>:headway"],  // which board
    ["subject", "Initial card title"],     // NIP-34 issue subject
    ["t", "label"]                          // optional inline labels
  ]
}
```

### Placement — kind `30620` (addressable)

Records which column a card is in and its fractional rank. Moving a card
republishes only this one event.

```jsonc
{
  "kind": 30620,
  "tags": [
    ["d", "headway:<issue-id-hex>"],      // board id : card id
    ["a", "30619:<author-hex>:headway"],  // board
    ["e", "<issue-id>"],                  // card
    ["col", "todo"],                       // target column id (or "__deleted__")
    ["rank", "m"]                          // fractional rank within the column
  ]
}
```

### Title edit — kind `1985` (NIP-32, `#subject` namespace)

```jsonc
{
  "kind": 1985,
  "tags": [
    ["e", "<issue-id>"],
    ["L", "#subject"],
    ["l", "New title", "#subject"]
  ]
}
```

### Labels — kind `1985` (NIP-32, `#t` namespace)

One `l` per label. Labels are an **additive union** (see below).

```jsonc
{
  "kind": 1985,
  "tags": [
    ["e", "<issue-id>"],
    ["L", "#t"],
    ["l", "bug", "#t"],
    ["l", "ux", "#t"]
  ]
}
```

### Description edit — kind `1624` (gitworkshop cover note)

```jsonc
{
  "kind": 1624,
  "content": "## markdown description",
  "tags": [
    ["e", "<issue-id>"],   // the card
    ["p", "<author>"],     // card author
    ["k", "1621"]          // kind being described
  ]
}
```

## Resolution

The reducer (`event::BoardReducer`) resolves the effective state of each card
from all its events using two rules:

- **Latest-authorised-wins** — placement (column + rank), title, and
  description: the newest *authorised* event wins, ties broken deterministically
  by author bytes.
- **Additive union** — labels: every *authorised* label set is unioned (then
  sorted + deduped). There is no "remove label" event yet, so labels are
  currently add-only (retraction is future work).

**Authority.** An overlay event counts only if its author is **the card's
author or the board's author (maintainer)**. The same rule gates every overlay
(placement, title, labels, cover note), so an unauthorised edit simply never
wins — no separate ACL machinery.

This latest-authorised / union scheme mirrors the gitworkshop "Shared Issue /
Patch / PR Metadata" spec and is what makes multi-author boards work without a
central server.

### Placement edge cases

- A card with **no placement**, or whose placement points at a column the board
  no longer defines, falls back into the **first column** (ordered by creation
  time). This is why deleting a column doesn't destroy its cards — they reflow.
- A placement whose `col` is the sentinel `COL_DELETED` (`"__deleted__"`) is a
  **tombstone**: the reducer drops that card. It's reversible (re-place the card
  to restore it) rather than a NIP-09 deletion.

## Ranking

Cards within a column are ordered by a **fractional rank** — lowercase `a`–`z`
strings compared lexicographically (`event::rank_between`). Inserting between two
cards mints a rank strictly between their neighbours, so a move/reorder
republishes exactly one placement and never reindexes the column.

Appending and inserting-between are unbounded (ranks just grow in length).
Repeatedly prepending walks toward the `"a"` floor; exhausting it needs a rank
rebalance (future work).

## Known limitations

- **Same-second ordering.** `created_at` is whole seconds, so two edits within
  the same second resolve by the author-bytes tiebreak rather than true
  recency. A logical clock / monotonic counter tag would make ordering airtight.
- **Label removal** isn't supported (union is add-only).
- **No relay sync** yet (local-only), and reloads after an edit use a short
  reload countdown rather than an ndb subscription.

## Source map

- `src/event.rs` — the pure schema: builders, parsers, the reducer
  (`reduce` / `load_board` / `BoardReducer`), `rank_between`. No I/O.
- `src/store.rs` — local-only persistence: sign + ingest (`ingest`), board
  seeding (`seed_default_board`), and `apply` which turns a `BoardAction` into
  events. **The single future home of relay publishing.**
- `src/lib.rs` — the `Headway` Notedeck `App`: loads a `BoardView` each frame,
  renders it, and collects `BoardAction`s.

Tracking issue: [damus-io/notedeck#1479][issue].

[NIP-34]: https://github.com/nostr-protocol/nips/blob/master/34.md
[NIP-32]: https://github.com/nostr-protocol/nips/blob/master/32.md
[issue]: https://github.com/damus-io/notedeck/issues/1479
