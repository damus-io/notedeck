# Collaborative editing — convergence spec

`draft` `spec`

> Scope: this is the **convergence layer** of cross-app collaborative editing on
> nostr — the deterministic fold that turns a bag of concurrent, attributed
> edits into one board/canvas every client agrees on. It is the sequel to the
> [gap analysis](./collab-editing-convergence-gaps.md) (which mapped where the
> current fold breaks) and the companion to
> [NIP-SNS](./nip-sns-sealed-shared-storage.md) (which carries the edits). Feeds
> `headway#way-buzz-raven`.

## Layering

Three layers, each specced separately, each replaceable without touching the
others:

1. **Transport & authorship** — [SNS](./nip-sns-sealed-shared-storage.md): a
   sealed shared-key channel where any keyholder publishes and every edit is
   cryptographically attributed to its real author. Done.
2. **Read-path fan-out** — the subscription that actually pulls every member's
   events to the reducer. Prerequisite; see [G5](#g5--read-path-fan-out-prerequisite).
3. **Convergence** — *this document*: given the events layers 1–2 deliver, the
   fold rules that make every client converge to the same state.

The convergence layer divides into rules that **must be shared** — if two
clients disagree, the same events fold to different states — and knobs that may
stay **app-specific**. This spec fixes the shared rules and names the app-specific
ones as declared options.

## The three settled decisions

The gap analysis closed on three open questions. Resolved:

| Question | Decision | Gap |
| --- | --- | --- |
| Logical clock | **HLC** (Hybrid Logical Clock) | [G1](#g1--hybrid-logical-clock) |
| Delete vs concurrent edit | **Add-wins** | [G4](#g4--add-wins-deletion) |
| Authority regime | **Both, declared per board** (`gate` \| `filter`) | [G6](#g6--authority-as-a-declared-regime) |

The rest of the spec is these three plus the "must be shared" determinism rules
(G2, G3, G7) the gap analysis had already recommended.

## Shared vs app-specific

| Concern | Regime | Where specified |
| --- | --- | --- |
| Clock & LWW comparison | **shared, mandatory** | [G1](#g1--hybrid-logical-clock) |
| Byte-ordering of all tiebreaks | **shared, mandatory** | [G7](#g7--canonical-tiebreak-ordering) |
| Delete/visibility policy | **shared, mandatory** | [G4](#g4--add-wins-deletion) |
| Additive-collection merge (labels, membership) | **shared, mandatory** | [G2](#g2--additive-collections-are-or-sets) |
| Fractional-rank tiebreak & rebalance | **shared, mandatory** | [G3](#g3--convergent-fractional-ranks) |
| Overlay *granularity* (how finely an app splits its overlays) | **app-specific** | apps choose |
| *Authority mode* (`gate` \| `filter`) | **app-specific, but declared** | [G6](#g6--authority-as-a-declared-regime) |

Rationale for the split: the first five are pure fold determinism — two clients
that disagree produce divergent state from identical events, which breaks the
premise of a shared board. Granularity and authority-mode do **not** break
convergence as long as every client folds the *same* declared mode identically,
so they can vary per app — provided each event/board declares which mode it
expects (never inferred).

## G1 — Hybrid Logical Clock

Replaces wall-clock `created_at` as the LWW sort key. An HLC gives a causal
total order while staying close to real time, so display ("updated 2w ago")
stays meaningful and a disconnected writer's later edit still out-ranks older
state without having *seen* it — the right fit for nostr's fold-when-received
model. It subsumes the `next_after` republish trick (`store.rs:787`) entirely.

### Wire format

Every **mutable overlay** event (headway: `1985`, `1624`, `30620`; notebook:
`31608`, `31609`, `31610`) carries one HLC tag:

```jsonc
["hlc", "<phys_ms>", "<logical>"]
// phys_ms : unsigned decimal, wall-clock milliseconds since the Unix epoch
// logical : unsigned decimal counter, reset to 0 whenever phys_ms advances
```

Immutable create-once events (headway issue `1621`, notebook node `1606`) need
no HLC — they are never in an LWW race with themselves.

### Comparison

LWW resolves overlays on the triple, most-significant first:

```
(phys_ms, logical, author_bytes)
```

`phys_ms` and `logical` compare numerically; `author_bytes` per
[G7](#g7--canonical-tiebreak-ordering). This replaces
`newer(a_at, a_who, b_at, b_who)` (`event.rs:2061`), which compared
`(created_at, author)`.

### Clock update (per client, local state)

Each client keeps one HLC `(phys, logical)` in its board state (persisted; not a
global — it lives in the app's board-state struct, per the no-globals rule). It
advances on **every** authored *and* ingested overlay, using the canonical HLC
algorithm:

```
// on authoring an overlay (send):
pt = max(phys, wall_ms())
logical = (pt == phys) ? logical + 1 : 0
phys = pt
// stamp the event with (phys, logical)

// on ingesting a remote overlay carrying (rp, rl):
pt = max(phys, rp, wall_ms())
if      pt == phys && pt == rp: logical = max(logical, rl) + 1
else if pt == phys:             logical = logical + 1
else if pt == rp:               logical = rl + 1
else:                           logical = 0
phys = pt
```

### Skew guard

A remote `rp` more than `MAX_SKEW` (recommended: 5 minutes) ahead of local
`wall_ms()` is **clamped** for the purpose of advancing the local clock (the
event is still folded, but it cannot drag our clock arbitrarily far forward).
This bounds a malicious or badly-skewed peer's ability to win all future races.

### Migration

Events without an `hlc` tag fall back to `(created_at·1000, 0)` so a legacy
overlay compares against HLC-stamped ones on the same millisecond axis. A client
that reads a legacy event advances its HLC from that fallback exactly as if it
had carried `["hlc", "<created_at·1000>", "0"]`. `next_after` is retired: a
republish just re-stamps a fresh, strictly-greater HLC.

## G4 — Add-wins deletion

Deletion/archival is an overlay carrying its own HLC, like any edit (headway:
placement to `__deleted__`/`__archived__`; notebook: a `del` transform). The
convergence rule biases toward **not losing work**:

> An item is **hidden** iff some tombstone overlay's HLC is **strictly greater**
> than the HLC of *every* content-bearing overlay of that item. Equivalently: an
> item is **visible** iff at least one content-bearing overlay has an HLC
> `≥` the newest tombstone.

Consequences:

- A concurrent edit an editor made without seeing the delete lands with an
  independent HLC; if it is `≥` the tombstone, the item **resurrects**. A
  resurrected card is visible and cheap to re-delete; a silently-buried
  concurrent edit is lost labor — the asymmetry the decision optimizes for.
- The delete-vs-edit decision is settled by HLC magnitude alone and **never**
  falls through to `author_bytes`: at equal HLC the **edit wins** (add-wins
  tie-favor). To *stay* deleted, a delete must be strictly the latest action on
  the item.

### Honest limitation

This is add-wins realized on a total order, not a full observed-remove OR-Set.
With only an HLC a client cannot distinguish "concurrent with" from
"causally-before" a delete, so a delete that genuinely happened-after an edit
but tied on HLC is treated as concurrent (edit wins). True causal add-wins would
need per-item version vectors; that is a **named non-goal** for v1 — the tie
window is a sub-millisecond same-`logical` collision and the failure mode
(an item survives one extra beat) is benign.

## G6 — Authority as a declared regime

The two apps mean different things by "authority," and a shared spec must not
force one. The board-def event (headway `30619`) declares the regime:

```jsonc
["authority", "gate"]    // or "filter"
```

The declaration changes **only** where authority is applied in the pipeline —
the fold math (G1–G4, G7) is identical under both modes:

- **`gate`** (headway today): an overlay enters the fold **only** if authored by
  an authorized key — the card author or the board maintainer
  (`relation_authorised`, `event.rs:1644`; `card_title`'s `authorised` closure,
  `event.rs:1631`). Unauthorized overlays are dropped *before* LWW; they can
  never win. This is the controlled-team-board guarantee.
- **`filter`** (notebook today): **all** overlays fold. Authority governs only
  *surfacing* — non-member versions land in a pending set
  (`CanvasView::pending`) and appear only once promoted or the canvas is opened.
  This is the open-canvas / promote-later model.

A client that does not recognize the declared regime MUST refuse to fold the
board rather than silently pick one — a wrong regime yields a different
converged state, which is exactly the divergence the declaration prevents. Under
SNS's implicit-membership v1 the "authorized set" is best-effort (any keyholder
can author); the [explicit roster](./nip-sns-sealed-shared-storage.md#membership--authority)
upgrade is what makes `gate` enforceable against removed members (see G6 note in
the gap analysis).

## G2 — Additive collections are OR-Sets

A whole-set snapshot overlay (headway labels: each `1985` carries the *complete*
label set) makes concurrent adds clobber — A adds `p1`, B adds `blocked`,
whichever set wins erases the other. Additive collections (labels, and later
membership) are specified as **per-element add/remove overlays** folded as an
OR-Set:

- an add and a remove of the *same element* race by HLC ([G1](#g1--hybrid-logical-clock)),
  add-wins on ties (consistent with [G4](#g4--add-wins-deletion));
- adds of *different* elements always union — no lost update.

Snapshot semantics are retained only where whole-value replacement is the
intended UX (e.g. a title, a description) and clobber is acceptable.

## G3 — Convergent fractional ranks

`rank_between` (`event.rs:2405`) mints a lexicographic rank strictly between two
neighbours. Two concurrent inserts into the same gap can mint equal or
interleaving ranks. Rules:

- **Intra-gap tiebreak.** When two placements resolve to equal rank strings,
  order them by `author_bytes` ([G7](#g7--canonical-tiebreak-ordering)) — the
  *same* deterministic ordering on every client. (Today equal ranks fall to the
  placement's LWW author tiebreak, which is unrelated to intended order; this
  makes the tiebreak an explicit, uniform part of the rank comparison.)
- **Rebalance** is a versioned **rank-epoch** on the board event: bumping the
  epoch reissues a fresh dense rank space. An epoch bump is itself an LWW overlay
  on the board (highest HLC wins), and placements name the epoch they were minted
  under; placements from a stale epoch sort before any current-epoch placement.
  Rebalance stays future work, but the epoch field is reserved now so it need not
  be a breaking change later.

## G5 — Read-path fan-out (prerequisite)

`headway_filter` (`event.rs:2086`) subscribes to a **single** author. Until the
subscription fans out across the roster there are no concurrent edits to
converge, so G1–G4 are unobservable. The required capability (specified with the
transport, not here): a member-aware subscription — the board `#a` coordinate
plus card `#e` ids across the roster — wired to SNS multi-`team_root`
trial-decrypt ([SNS §nostrdb integration](./nip-sns-sealed-shared-storage.md#nostrdb-integration)).
This spec assumes every member's overlays reach the reducer.

## G7 — Canonical tiebreak ordering

Every tiebreak in the fold — overlay LWW author tiebreak, OR-Set element ties,
intra-gap rank ties, comment/archived ordering — MUST use **one** byte ordering:
unsigned lexicographic comparison over the raw 32-byte arrays (pubkey for author
tiebreaks, event id where an id tiebreak is called for). Today author tiebreaks
(`event.rs:2061`) and id tiebreaks (comments/archived) coexist; the spec keeps
both *kinds* but fixes each to this single, platform-independent byte order so
every client folds identically.

**Text granularity** is an explicit **non-goal**: description/content is
whole-field LWW ([G1](#g1--hybrid-logical-clock)); two concurrent prose edits
lose one side. A character-level text CRDT is out of scope for card
descriptions — named here so it is a decision, not an accident.

## Implementation sequence

Unchanged from the gap analysis's severity ranking, now with decisions attached:

1. **G5** — read-path fan-out. Nothing is observable until members' events converge.
2. **G1** — HLC clock + comparison; retires `next_after`. Unblocks a principled G4.
3. **G2** — OR-Set labels (stop silent lost updates).
4. **G4** — add-wins visibility rule (cheap once G1 lands).
5. **G3** — rank tiebreak now; rank-epoch field reserved, rebalance later.
6. **G6 / G7** — declared `authority` tag + roster-gated enforcement; canonical
   byte order. Largely spec-text + a validation gate.

## References

- [`docs/collab-editing-convergence-gaps.md`](./collab-editing-convergence-gaps.md) — the gap analysis this resolves
- [`docs/nip-sns-sealed-shared-storage.md`](./nip-sns-sealed-shared-storage.md) — transport/authorship layer
- `crates/notedeck_headway/README.md` — headway convergence model + known limitations
- `crates/notedeck_notebook/src/event.rs` — notebook convergence model
- `crates/headway/src/event.rs` — the reducer (`newer`, `rank_between`, `headway_filter`)
