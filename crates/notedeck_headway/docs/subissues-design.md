# Headway subissues: structured parent/child cards

Status: implemented (model, store, CLI and GUI)

## Motivation

Epics on the board today are held together by hand: an `epic` label, a
description containing a markdown checklist of `headway#word-id` references,
and discipline. That gives us none of the things a tracker should derive for
free:

- **No rollup.** "How far along is the imgproxy epic?" means opening the epic,
  opening each referenced card, and diffing the checklist against reality.
- **Stale checkboxes.** The `- [ ]` state in the epic body is a second copy of
  the child's column and drifts the moment a child moves to Done.
- **No backlinks.** A child card doesn't know it belongs to an epic; there's
  nothing to render, filter or navigate on.
- **Fragile references.** Word-ids in free text survive, but nothing validates
  them, and re-scoping a card in or out of an epic is a text edit.

## Goals

- A card can have **one parent** and any number of children (GitHub sub-issue
  semantics).
- Parent/child is **mutable**: re-parent and detach under the same
  latest-authorised-wins rules as every other card overlay.
- **Progress is derived, not stored**: a child is done because of where it sits
  on the board, not because someone ticked a box.
- Works across boards: children may be placed on a different board than the
  parent (multi-board membership is placement-driven and stays untouched).
- Backward compatible: no change to existing kinds, no migration; boards
  without relations fold exactly as before.

## Non-goals (for now)

- Arbitrary DAGs / multiple parents. One parent per child, like GitHub.
- Recursive rollups (grandchildren counting toward the grandparent). The view
  models one level; nesting still *works* (a child can itself be a parent) but
  each rollup is over direct children only.
- Blocking/depends-on relations. Different concept, different spike.
- Checklist-style "lightweight" subtasks that aren't real cards. Every subissue
  is a kind-1621 issue; if it's worth tracking it's worth a card.

## Wire format

One new addressable kind, following the placement (30620) pattern:

| concept  | kind    | mechanism                                        |
| -------- | ------- | ------------------------------------------------ |
| relation | `30621` | addressable; `d` = child issue id, `parent` tag |

```json
{
  "kind": 30621,
  "tags": [
    ["d", "<child-issue-id-hex>"],
    ["e", "<child-issue-id>"],
    ["parent", "<parent-issue-id>"]
  ],
  "content": ""
}
```

- `d` = the **child** issue id. This is the load-bearing choice: the relation
  is *child-side*, so each child has exactly one relation slot. Relays replace
  addressable events per `d`, and the reducer applies latest-authorised-wins
  within the slot — identical semantics to a placement.
- `e` duplicates the child id as a real id tag so `#e` filters work when
  boards go collaborative.
- `parent` carries the parent issue id. **Omitting the tag detaches**: a newer
  relation event with no `parent` tag clears the child's parent (mirrors how
  republishing a label set without a label removes it).

Why child-side rather than a children-list on the parent:

- One-parent-per-child is enforced structurally by the `d` key instead of by
  merge logic.
- Concurrent "add subissue" edits touch different `d` slots and can never
  clobber each other; a parent-side list is a single register that loses
  writes on concurrent append.
- Re-parenting is one event on one slot, and it composes with authority rules
  the same way placements do.

The trade-off is that child *ordering* within a parent isn't expressed. If we
want manual ordering later, the relation gains a `rank` tag using the existing
fractional-rank scheme — same slot, no new kind. Until then children sort by
`(created_at, id)`.

## Semantics

**Authority.** A relation is honoured if its author is the child's author, the
parent's author, or the board author — the same authorised set as the other
overlays, extended to both endpoints of the edge. (Single-author boards make
this moot today; it matters when boards go collaborative.)

**Latest wins.** Newest authorised relation per child wins, ties broken by
author bytes — the same `newer()` comparator as placements/subjects/labels.
As with every overlay, ingest is authority-blind and authority is applied at
resolve time: an unauthorised newer event shadows an older authorised slot
(the card reads as unparented) rather than being outranked by it. This matches
the existing subject/label semantics; single-author boards never hit it.

**Doneness is positional.** A child counts as *done* when every live placement
it has sits in the **last column of its board** ("Done" on the default board),
or when it is archived everywhere it's placed (archival here reads as
"resolved and filed away"). A child with no live placement (an orphan, or
never placed) is not done. There is no per-child checkbox state anywhere — the
board is the source of truth, which is exactly what the manual checklists got
wrong.

**Deletion.** A child that is tombstoned off every board disappears from its
parent's subissue list, same as it disappears from boards. Restoring it
(re-placing) brings it back — the relation slot itself was never touched.

**Cycles.** The write path (`store::apply`) refuses to set a parent that would
create a cycle (it walks the ancestor chain first). The read path doesn't need
a guard: the reducer only ever materialises one level (direct parent pointer +
direct children), so a malicious or racy cycle renders as two cards pointing
at each other rather than an infinite loop.

**Cross-board.** Relations are board-agnostic, like titles and labels: the
edge names two issue ids, and each side renders wherever it happens to be
placed. An epic on `headway` can have children living on `work`.

## View model

`CardView` grows:

```rust
pub parent: Option<NoteId>,
pub subissues: Vec<SubissueView>,   // direct children, resolved

pub struct SubissueView {
    pub id: NoteId,
    pub title: String,          // subject overlay applied
    pub column: Option<String>, // live column id (prefers the board being rendered)
    pub done: bool,
    pub archived: bool,
}
```

The reducer accumulates `relations: HashMap<child, RelationEvent>` and resolves
both directions at `card_view` time. Children resolution scans the relation and
placement maps per parent — O(cards × relations) worst case, same order as the
existing per-board placement walk; fine at board scale, and cacheable later if
it ever shows up.

## CLI surface

```bash
headway add "wire up the parser" --col todo --parent <epic>   # create as subissue
headway parent <card> <epic>       # (re)parent an existing card
headway parent <card>              # no parent argument = detach
headway show                       # parents show a dim n/m progress counter
headway show <epic>                # detail gains: parent line, subissues section
```

`show <epic>` renders the derived checklist the description used to fake:

```
subissues (2/4 done)
    [x] route media loads through imgproxy      headway#mushroom-include-wolf
    [x] request only the resolution each view…  headway#scheme-ask-exercise
    [ ] fetch media lazily / on-demand          headway#demise-deny-glass
    [ ] cap media cache size with eviction      headway#extend-decrease-visit
```

`--json` gains `parent`, `parent_words` and `subissues` on each card.

## GUI

- Card front: a compact `2/4` progress pill when a card has children, and a
  small `↳` marker on children.
- Card detail: a subissues section — the derived checklist (read-only
  checkboxes, click a child to open it, muted column/archived hints) with an
  inline "Add subissue…" composer that creates the card + relation in one
  action (`AddCard { parent }`, landing in the first column) — and a
  "↳ subissue of" breadcrumb above the title (click to open the parent, ✕ to
  detach). A parent placed only on another board renders as its word-id.
- Context menu: a "Set parent" submenu mirroring the move/link-to-board picker,
  filtered with the same cycle guard the store applies on write, plus a
  "Detach from parent" entry.

## Alternatives considered

- **Keep labels + description checklists** (status quo): no rollup, stale
  state, no backlinks — the motivating pain.
- **Parent-side children list event**: loses concurrent appends, duplicates
  ordering that placements already own, and makes one-parent unenforceable.
- **A board per epic** (multi-board membership already allows it): boards are
  workflow surfaces, not grouping; you'd lose the child's position in the main
  flow, and rollup across boards is the same unsolved problem.
- **NIP-51 lists**: replaceable list-of-e-tags per parent — same register
  problem as the parent-side list, plus a second authority model to reconcile.

## Open questions

- Should archived-everywhere really count as done? (Current call: yes —
  archival in practice is filing away finished work. Revisit if archive grows
  an "abandoned" flavour.)
- Should relation edits bump the child's (or parent's) `updated_at`? Currently
  they don't; a reparent is invisible to recency sorting.
- Manual child ordering (`rank` on the relation) — deferred until a real need.
- Whether the board view should mark children (`↳`) as well as parents (`n/m`)
  — deferred to the GUI pass to avoid cluttering the CLI board listing.
