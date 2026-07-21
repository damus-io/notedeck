# Collaborative editing — convergence gap analysis

`draft` `analysis`

> Scope: this is a **gap analysis**, not a spec. It maps where the convergence
> model the Notedeck apps already implement breaks (or is merely unspecified)
> under genuine multi-writer concurrency, so the eventual cross-app
> collaborative-editing spec can target real problems. Feeds
> `headway#way-buzz-raven` ("Figure out collaborative editing spec").

## Two layers

Collaborative editing over nostr splits cleanly into two independent layers:

1. **Transport & authorship** — how a private, multi-writer channel carries
   attributable edits. This is specced in
   [`docs/nip-sns-sealed-shared-storage.md`](./nip-sns-sealed-shared-storage.md)
   (SNS): a sealed shared-key channel where any keyholder can publish and every
   edit is cryptographically attributed to its real author, with key rotation
   for membership. This layer is in good shape.

2. **Convergence** — given a bag of concurrent, attributed edits from several
   members, what board/canvas does everyone end up seeing? The SNS doc defers
   this entirely to "the reducer" (SNS §Reading workflow, §nostrdb
   integration). **This document is about layer 2.**

## The convergence model today

Both collaborative apps are event-sourced and share one skeleton — a
deterministic, order-independent reducer folding append-only overlays:

- **headway** (`crates/headway/src/event.rs`, `crates/notedeck_headway`): an
  immutable issue (`1621`) plus small overlays — placement (`30620`), title /
  labels (`1985`), description (`1624`).
- **notebook** (`crates/notedeck_notebook/src/event.rs`): an immutable node
  (`1606`) plus finer overlays — transform/geometry (`31608`), content
  (`31609`), edge (`31610`).

The shared resolution rule is **latest-wins per overlay, ties broken by author
bytes**:

```rust
// crates/headway/src/event.rs:1604
fn newer(a_at: u64, a_who: &[u8; 32], b_at: u64, b_who: &[u8; 32]) -> bool {
    (a_at, a_who) > (b_at, b_who)
}
```

Card/node **order within a lane** is a fractional rank (`rank_between`,
`crates/headway/src/event.rs:1732`; mirrored in notebook), so a reorder
republishes one placement and never reindexes the lane.

### Where the two apps already diverge

Same skeleton, but three load-bearing decisions differ — a cross-app spec must
reconcile these, because today "collaborative editing" means two different
things:

| Decision | headway | notebook |
| --- | --- | --- |
| **Authority** | *validity gate*: an overlay counts only if authored by the card author or board maintainer; unauthorised edits never win (`notedeck_headway/README.md` §Resolution) | *visibility filter*: anyone may append; owner+members control only what's **surfaced**; others land in `CanvasView::pending` until promoted or the canvas is opened (`notedeck_notebook/src/event.rs:25-33`) |
| **Overlay keying** | one latest-authorised slot per overlay (the whole board folds to a single winner per field) | per-`(element, author)` map — each author's latest version is retained, bounded by collaborator count (`event.rs:35-38`) |
| **Granularity** | title+labels share kind `1985`; labels are a whole-set snapshot | geometry vs content split into separate kinds so concurrent move + text-edit merge with no lost update (`event.rs:21-23`); plus an ephemeral presence kind never folded (`event.rs:59-60`) |

## Gap catalog

| # | Gap | Where | Anomaly under concurrency | Severity |
| --- | --- | --- | --- | --- |
| G1 | Wall-clock LWW | `event.rs:1604` | clock skew ⇒ fast clock always wins; same-second cross-author ⇒ author-bytes decides, not recency | High |
| G2 | Snapshot overlays clobber | headway labels (`1985`) | concurrent relabel ⇒ one author's whole set silently replaces the other's | High |
| G3 | Fractional rank convergence | `rank_between` | concurrent insert into the same gap ⇒ colliding/interleaved ranks; prepend floor exhaustion; no rebalance | Medium |
| G4 | Delete vs concurrent edit | placement/transform tombstones | delete is just another LWW overlay ⇒ a later-clock edit resurrects a deleted item (or vice-versa); add-wins/remove-wins unspecified | Medium |
| G5 | Read-path fan-out | `headway_filter` (`event.rs:1628`) | single-author subscription; other members' events are never pulled, so nothing to converge | High (blocks everything) |
| G6 | Authority under rotation & across apps | SNS §Membership; app authority models | removed member's pre-rotation edits still count (implicit membership); headway validity-gate vs notebook visibility-filter disagree | Medium |
| G7 | Tiebreak / granularity determinism | `event.rs:1604` vs `:1418` | overlays tiebreak on author-bytes, comments/archived on id-bytes; whole-field text LWW ⇒ concurrent prose edits lose one side | Low–Medium |

## Detail

### G1 — Wall-clock LWW is not a causal order

`created_at` is whole-second wall-clock, and the winner is `(created_at,
author)`. Two independent failures:

- **Skew.** Member A's clock runs 3s fast. Every A edit outranks a B edit made
  slightly later in real time. A systematically wins races it didn't causally
  win.
- **Same-second cross-author.** A and B both retitle a card in the same second.
  `created_at` ties, so the *lexicographically larger author pubkey* wins —
  independent of who edited last. The `next_after` causal-stamp trick
  (`store.rs:663`) bumps a republish one second past the state it supersedes,
  but that only orders **one author's own** successive edits; it does nothing
  across authors (`notedeck_headway/README.md:222-230` says as much).

*Candidate fix:* carry a logical clock — a Lamport counter or HLC — in a tag,
and resolve on `(logical_clock, author)`. This is the general fix the headway
README already flags. It subsumes `next_after`.

### G2 — Snapshot overlays silently drop concurrent edits

headway labels are a whole-set snapshot: each `1985` event carries the card's
*complete* label set, newest-wins (`notedeck_headway/README.md:140-156`,
`:231-232`). So if A adds `p1` and B adds `blocked` concurrently, whichever set
wins **erases the other label entirely** — a lost update, not a merge.

notebook mostly dodges this by splitting overlays finely and keying per author,
but any whole-value overlay has the same hazard.

*Candidate fix:* model additive collections (labels, membership) as an OR-Set /
2P-Set of per-label add/remove overlays rather than a snapshot, so concurrent
adds union. Keep snapshot semantics only where clobber is acceptable.

### G3 — Fractional ranks don't converge under concurrent insert

Ranks are lexicographic `a`–`z` strings; an insert mints a rank strictly between
its neighbours (`rank_between`, `event.rs:1732`). Two hazards:

- **Concurrent insert into the same gap.** A and B both drop a card between the
  same two neighbours. They independently mint ranks in the same interval; the
  results may be equal (then the card LWW/author tiebreak on the *placement*
  decides, which is unrelated to intended order) or interleave in an order
  neither user chose. Cards still converge to *a* deterministic order, but not a
  *meaningful* one.
- **Floor exhaustion / no rebalance.** Repeated prepends walk toward the `"a"`
  floor; there's no rebalance protocol (`notedeck_headway/README.md:217-219`
  calls it future work), and a rebalance is itself a multi-writer event that
  needs its own convergence story.

*Candidate fix:* append author bytes to the rank as an intra-gap tiebreak so
equal gaps order deterministically the same way everywhere; specify an
interleaving rule; define a convergent rebalance (e.g. a versioned rank-epoch on
the board event).

### G4 — Delete vs concurrent edit is unspecified

Deletion is not special: it's a placement to the `__deleted__` sentinel
(`notedeck_headway/README.md:199-201`) / a `del` flag on a transform overlay —
just another LWW overlay. So a delete and a concurrent edit race by clock: an
edit with a later logical clock **resurrects** a deleted card; a later delete
**buries** a concurrent edit. Neither add-wins nor remove-wins is stated, and
restore (`__archived__` + `from`) adds a third racing overlay.

*Candidate fix:* pick and document a policy (add-wins is the usual friendly
default for shared boards), and make tombstones dominate edits with an
equal-or-lower clock explicitly rather than by accident of ordering.

### G5 — The read path is single-author (blocks everything)

`headway_filter` subscribes to **one** author's events
(`event.rs:1628-1633`); the comment there notes collaborative boards "will
additionally need `#a`/`#e` filters to pull in other authors' events." Until the
subscription fans out across members (and, under SNS, trial-decrypts across
every held `team_root`), other members' overlays never reach the reducer — so
there is nothing to converge. This is a prerequisite for G1–G4 mattering at all.

*Candidate fix:* member-aware subscription (`#a` board coordinate + `#e` card
ids across the roster), wired to SNS multi-root trial-decrypt
(SNS §nostrdb integration).

### G6 — Authority under rotation, and across apps

Two sub-gaps:

- **Removed members.** SNS v1 is implicit membership — possession of `team_root`
  *is* membership (SNS §Membership, v1). A rotated-out member keeps the old root
  and can still read history; more importantly, their **pre-rotation edits stay
  in the log and still win LWW**. Enforcing "a former member's edits stop
  counting" needs the admin-signed roster SNS lists as a TODO, plus a rule tying
  overlay validity to roster-membership-at-clock.
- **App disagreement.** headway's authority is a *validity gate*; notebook's is a
  *visibility filter*. A shared spec must either unify these or explicitly offer
  both as modes, because they produce different converged states from the same
  events.

### G7 — Determinism and granularity nits

- **Tiebreak consistency.** Overlay LWW tiebreaks on **author bytes**
  (`event.rs:1604`), but comment and archived ordering tiebreak on **id bytes**
  (`event.rs:1418`, `:1568`). Both are deterministic, but the spec should state
  each explicitly and require identical byte-ordering across apps/platforms so
  every client folds identically.
- **Text granularity.** Description/content is whole-field LWW; two concurrent
  prose edits lose one side entirely. A character-level text CRDT is almost
  certainly overkill for card descriptions but should be a *named* non-goal, not
  an accident.

## Cross-app reconciliation — what must be shared vs app-specific

- **Must be shared** (or clients diverge): the clock/tiebreak rule (G1, G7), the
  delete policy (G4), the rank tiebreak & rebalance (G3). These are pure fold
  determinism — if two apps disagree, the same events yield different states.
- **Can stay app-specific**: overlay *granularity* (how finely each app splits
  its overlays) and the *authority mode* (validity gate vs visibility filter),
  **provided** the spec names both as first-class options and each event
  declares which regime it expects.

## Severity-ranked recommendation

1. **G5** — read-path fan-out. Nothing else is observable until members' events
   converge. Prerequisite.
2. **G1** — logical clock. The single change that makes LWW causal and retires
   the `next_after` workaround; unblocks a principled G4.
3. **G2** — additive-collection merge (labels/membership) to stop silent lost
   updates.
4. **G4** — written delete policy (cheap once G1 lands).
5. **G3** — rank tiebreak + rebalance (order stays *converged* today, just not
   always *meaningful*, so lower urgency).
6. **G6 / G7** — roster-gated authority and determinism nits; largely spec-text.

## Open questions

- Lamport vs HLC for G1 — HLC keeps timestamps human-meaningful for display; is
  that worth the extra tag complexity?
- Is add-wins the right universal default for G4, or should destructive ops be
  roster/admin-gated (ties into G6)?
- Should the cross-app spec mandate one authority regime, or standardise both as
  declared modes?

## References

- [`docs/nip-sns-sealed-shared-storage.md`](./nip-sns-sealed-shared-storage.md) — transport/authorship layer
- `crates/notedeck_headway/README.md` — headway convergence model + known limitations
- `crates/notedeck_notebook/src/event.rs` (module docs) — notebook convergence model
- `crates/headway/src/event.rs` — the reducer (`newer`, `rank_between`, `headway_filter`)
