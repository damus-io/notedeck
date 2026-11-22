---
name: headway
description: Read and edit a Headway kanban board from the command line via `nd headway` (the headway tools of the running notedeck, crates/notedeck_headway). Use when the user wants to view the board, add/move/edit/archive cards, or shuffle work between columns like Backlog, Todo, In Progress, In Review, and Done — e.g. "move X to done", "show the board", "add a card to todo".
---

# Headway boards from the command line: `nd headway`

`nd headway <command>` runs one of the running notedeck's headway tools over
its local socket (see the `nd` skill). The notedeck folds the board from its
own nostrdb, signs edits as its selected account and publishes them, so the app
and every `nd` call see the same board with nothing to sync. The commands and
their text output live in `crates/notedeck_headway/src/tools.rs` (and
`tools/`); `nd` itself (`crates/nd_cli`) only builds the command line from the
tool list.

## Running it

```bash
nd headway show --board notedeck             # nd on PATH (scripts/install-clis)
cargo run -q -p nd_cli -- headway show       # or from a checkout
```

With no notedeck running, nd starts a data-only headless one itself (see the
`nd` skill), so a call never fails for want of the GUI.

`nd headway --help` lists the commands. Each has its own page, `nd headway
<command> --help`, with its arguments and the tool it runs; `nd help options`
has the options every run takes and the exit codes. Every command also takes
`--json` (print the tool's answer as JSON instead of its text),
`--pager`/`--no-pager` and `--color <auto|always|never>`.

`headway <command>` still works through a shim (`scripts/shims/nd-shim`) that
runs `nd headway <command>`, but type `nd headway` in anything new.

## Identity

There is no login. Every edit is signed by the account selected in the
notedeck that answers the socket, and every read is that account's view.
`--nsec`, `--relay`, `--db` and `--author` are gone with the old CLI. To work
against a private notedeck (a test, a rig), point `NOTEDECK_SOCKET` at its
socket. If nd can't reach or start a notedeck it exits 2; tell the user rather
than handling keys yourself.

## Multiple boards

A board is identified by a slug scoped to its owner's key, so one identity can
hold several boards (e.g. a personal `headway` board and a `work` board).

**Name the board on every command**, with `--board <id>` or a card ref that
names it (below). There is no persisted current board any more: `nd headway
board <id>` refuses the old switch and says so. With neither, a command falls
back to `$HEADWAY_BOARD`, then the primary board.

```bash
nd headway show --board work
nd headway add "Fix the relay reconnect" --board work --col todo -l bug
nd headway move 1a2b3c4d… --board work --col done
```

A flag may also go before the verb (`nd headway --board work show`).

**Full card refs self-route.** A selector like `headway:commerce/purse-metal-toilet`
already names its board, so `nd headway show headway:commerce/purse-metal-toilet`
(and `move`, `comment`, …) targets `commerce` with no `--board`. The
`headway:<board>/<word-id>` that `show` prints is a working address wherever you
paste it. The scheme-less shorthand `commerce/purse-metal-toilet` self-routes
too. A hex prefix or a **bare word-id** (`purse-metal-toilet`, no board
segment) resolves against the board picked, so `nd headway show
purse-metal-toilet --board commerce` works. A bare word-id also matches by
**unique prefix** (`purse-metal`, or even `purse`). Anything that isn't purely
hex digits is treated as a word-id, so only an all-`0-9a-f` selector is a hex
prefix. When a guess misses, the error suggests the real cards that share its
leading word. A ref that disagrees with an explicit `--board` is an error, not a
silent resolution on the wrong board.

`nd headway board` lists the boards the account can see: its own, the ones
shared with it (each with its owner), and its deleted ones (restorable with
`restore-board`). Use it to discover slugs. To create a board, `nd headway seed
--board work`, or `nd headway seed -t "Tune Assistant"` to derive the slug
(`tune-assistant`) from the title. `seed` never falls back to `$HEADWAY_BOARD`:
with neither `--board` nor `--title` it refuses, like `next` and `migrate`.
`delete-board <board>` and `restore-board <board>` take the board outright,
never defaulted.

## The golden rule: `show` before you edit

Cards are addressed by their **event id**, and columns by **id or
case-insensitive name**. When scripting, pass a hex `id` from `show --json`.
Any unique prefix resolves, so the full 64-char id is overkill: a **16-char
(8-byte) prefix** is plenty, and even an 8-char prefix is usually unambiguous.
Lengthen it if you ever hit an "ambiguous card prefix" error. The
human-readable `show` instead displays a muted **reference** like
`headway:headway/maple-river-canyon` (a friendly rendering of that same event
id, for quoting in commits and chat); it also resolves as a `<card>` argument.
Always run `show` first to read the current ids and column names, then act on
what you actually see — never assume an id or that a card is where you expect.

```bash
nd headway show --board b              # human-readable: columns, titles, labels, word-ids
nd headway show --board b --archived   # also list archived cards in full (default: count only)
nd headway show --all                  # every board this account can read, each led by its slug
                                       # (with --json, a JSON array of boards)
nd headway show --board b --json       # machine-readable, for parsing (always includes archived)
nd headway show <card>...              # the given cards in full, `git show`-style; with
                                       # --json each card gains a `column` field
```

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
- a bare `maple-river-canyon` (no scheme, no board) — doesn't resolve on its own.
- the scheme-less `dave/maple-river-canyon` — fine as a command argument, but it
  does **not** linkify in prose; always add the `headway:`/`notebook:` scheme
  when writing for a human.

Apply this to **every** ref in a message, not just the first. When *you* edit
the board (move, label, archive, …), the hex id from `show --json` or the full
ref are both safe, since each names exactly one card.

Default board columns: **Backlog**, **Todo**, **In Progress** (`in-progress`),
**In Review** (`in-review`), **Done** (`done`). A column argument matches an id
or a name case-insensitively, so `--col "in progress"`, `--col in-progress`, and
`--col "In Progress"` are equivalent.

## Commands

Every command below is `nd headway <command>`. Card commands also take
`--board <id>`; see each one's `--help`.

| Command | What it does |
| --- | --- |
| `show [cards...] [--archived] [--all] [--json]` | Print the board, or only the given cards (`--archived` lists archived cards; `--all` prints every board) |
| `board` | List the boards this account can see (own, shared, deleted) |
| `seed [--board <slug>] [-t\|--title <t>]` | Create a board (born sealed): the `--board` slug, else one derived from `--title`; refuses with neither |
| `rename <title...> --board <id>` | Rename a board's display title (slug unchanged) |
| `delete-board <board>` / `restore-board <board>` | Delete a board you own (reversible), or bring it back |
| `add <title...> [--col <c>] [-l <labels>] [--parent <card>] [--desc <text>\|--desc-file <path>] [--priority <p>] [--estimate <n>] [--due <date>]` | Add a card (defaults to the first column); `--json` answers its `ref` |
| `move <card> --col <c> [--row <n>]` | Move a card to a column (optional position); `--to left\|right\|up\|down` steps it one place instead |
| `title <card> <title...>` | Edit a card's title |
| `desc <card> <text...>` / `desc <card> --desc-file <path\|->` | Replace a card's description |
| `label <card> [labels...]` | Set labels — positional, comma-separated, or `-l` (no labels clears them) |
| `priority <card> <level>` | Set priority: `none`/`low`/`medium`/`high`/`urgent` (`none` clears it) |
| `parent <card> [parent]` | Make a card a subissue of `[parent]`; omit the parent to detach |
| `block <card> --on <other>` | Mark `<card>` as blocked by `<other>` (see Dependencies) |
| `unblock <card> --on <other>` | Remove the `<card>`-blocked-by-`<other>` edge |
| `relate <card> --to <other>` | Relate two cards (an undirected "see also"; see Dependencies) |
| `unrelate <card> --to <other>` | Remove the relation (from either endpoint) |
| `due <card> <date>` | Set a due date (`YYYY-MM-DD`, or `none` to clear) |
| `estimate <card> <n>` | Set an estimate — a number (or `none` to clear) |
| `seq <card> [--in <c>] [--json]` | Print where a card sits in a container's work-order (see Work order) |
| `seq <card> --first\|--last\|--after <c>\|--before <c> [--in <c>]` | Position a card in a container's work-order |
| `next [--in <c>] [--ready] [-n <k>] [-l <labels>]` | Print the ready frontier — what to work on next (see Work order) |
| `grep <pattern> [--board <b>] [--in <c>] [-i\|-s] [--json]` | Search card text (title, description, comments, review comments) and print each matching line under its card's ref (see Searching cards) |
| `list [query...] [--group …] [--sort …] [--view <v>] [--json]` | List cards across boards by a filter query, grouped and sorted; `--save-view`/`--delete-view`/`--views` manage saved views (`nd headway list --help`) |
| `comment <card> <text...> [--reply-to <c>]` | Comment on a card (NIP-22); `--body-file <path\|->` reads the text; `--reply-to` threads under another comment or a review comment |
| `comment <card> --path <f> --line <a[-b]> [--old] [--record <sha>] <text...>` | Inline review comment on a review record's commit (newest record by default); `--record` alone comments on the whole commit |
| `comment <card> --attach <image> <text...>` / `desc <card> --attach <image> <text...>` | Attach screenshots: each file is encrypted, uploaded to the account's kind-10063 Blossom server, and shown inline (a comment gets its URL on a line, a description a markdown image); repeat `--attach` per file. Fails with no Blossom list |
| `attach <image...>` | Just upload: prints each attachment's URL; `--json` gives the `attachments` a comment or desc takes |
| `review <card> [--explainer <url>] [--explainer-source <f>] [--code-source <f>] [--deploy <url>] [--commit <rev>]` | Record a commit for review on the card: sha, title, branch, host, repo path and `agentium:` ref, read from git in `--repo-dir` (default cwd) and `$AGENTIUM_SESSION` |
| `diff <card> [--record <sha>] [--note explain\|code]` | Print the commit the card's newest review record names (`git show`-style), fetching it from the recording host if this one lacks it; falls back to the card's `Headway:` trailer. `--note` prints the commit's explain git note instead |
| `delete <card>` | Remove a card (reversible tombstone) |
| `archive <card>` | Archive a card off the board |
| `restore <card>` | Restore an archived card |
| `link <card> --to <board>` | Also place the card on another board (it stays on this one) |
| `move-board <card> --to <board>` | Move the card off this board onto another |
| `share <npub\|hex> --board <id>` | Make someone a member of a sealed board you own (see Sharing a board) |
| `migrate --board <id> [--dry-run]` | Seal a plaintext board (never on a real board casually) |
| `terminal <col> [on\|off] --board <id>` | Mark a column as a "done" column, or clear it with `off` (see Terminal columns) |

`add` accepts `-l`/`--label` to tag the new card in one step. The flag is
repeatable and each value may be comma-separated, so `-l a,b --label c` and
`-l a -l b -l c` are equivalent:

```bash
nd headway add "Fix the relay reconnect" --board b --col todo -l bug,p1
```

`label <card>` takes the same spellings — separate positionals, one
comma-separated positional, or the `-l`/`--label` flag. It *replaces* the
card's set rather than adding to it, and `label <card>` with no labels at all
is what clears:

```bash
nd headway label 1a2b3c4d… bug p1
nd headway label 1a2b3c4d… bug,p1
nd headway label 1a2b3c4d… -l bug,p1
nd headway label 1a2b3c4d…              # clears
```

`add` can also set the new card's description at creation. `--desc <text>`
takes it inline; `--desc-file <path>` reads it from a file, or from stdin when
`<path>` is `-`, so a long multi-line markdown description can heredoc in with
no shell-escaping. `nd` reads the file on the caller's side, relative to the
current directory. Both compose with `--col`/`-l`/`--parent`. `add --json`
answers `{ok, events, card, ref}`, so a script can take the new card's ref with
`jq -r .ref`:

```bash
nd headway add "Rework the sync engine" --board b --col todo --desc "Backfill stalls past the maxSyncEvents cap."
ref=$(nd headway add "Migration plan" --board b --col todo --json --desc-file - <<'EOF' | jq -r .ref
## Migration plan

1. Fork the session loop
2. Retire the per-app poller
EOF
)
```

**Every new card says where it came from.** When a Dave agentic session creates
a card, the description **starts** with a backlink to that session, so whoever
reads the card later can open the transcript that produced it instead of hunting
for it:

```bash
nd headway add "Cap the media cache" --board b --col todo --desc-file - <<EOF
Created from $AGENTIUM_SESSION while profiling image loads.

Eviction never runs past 2 GB …
EOF
```

`$AGENTIUM_SESSION` is your own `agentium:<word-id>` (see the `agentium` skill
for the fallback when it's unset). This applies to *every* `add` — follow-ups,
subissues from an epic breakdown, cards filed in passing — and only at
creation; later edits to the description keep the line rather than re-adding it.
Outside a session, name the card or conversation it came from instead (e.g.
`Split out of headway:dave/maple-river-canyon`).

`--on <card>` names the blocker for `block`/`unblock`; `--to` is the target
board for `link`/`move-board` and the partner card for `relate`/`unrelate`;
`--in <c>` is the container for `seq`/`next`/`grep`/`list`.

When a card's work is committed, record it with `nd headway review <card>` from
the checkout (plus `--explainer <url>` when there is one, and `--deploy <url>`
when the commit's build was deployed somewhere it can be opened and tried)
before the done comment. nd reads git in the caller's checkout (`--repo-dir`,
default the current directory), not the notedeck's. The record is the
machine-readable twin of that comment: headway reads it to show the commit's
diff, and the app's review pane links the explainer and the deployed build
beside it. When the explainer was written as explain Markdown, pass its source
too: `--explainer-source ~/explainers/<word-id>/rec/<word-id>.md` (the
`explain inline` output) and, for a change with a code walk,
`--code-source ~/explainers/<word-id>/<word-id>.code.md`. Give the paths in
full; one that won't read refuses the whole record. Each is stored on the
record under its file name, sealed with the board like any card content, and
`show --json` returns it in the review's `sources`, which is where `explain
book` builds the card's chapter from (`show <card> --source <file>` prints one).
The `visual-explainer` skill says how to write both. `show <card>` lists a
card's records under **review**, and `nd headway diff <card>` prints the commit
itself: it looks in the recorded checkout, then any checkout of the same repo
(matched by root commit), then a headway-owned bare cache, and fetches over ssh
from the recording host when none has it — never prompting, never creating
refs in your checkout. `explain` also writes the commit's git notes
(`refs/notes/explain` for the page, `refs/notes/explain-code` for the walk);
`nd headway diff <card> --note explain|code` prints one, read from the same
place as the commit. Colleagues without headway read them with `git log
--notes=explain --notes=explain-code` after fetching `refs/notes/explain*`.

Inline review comments left on a commit (in the app's review pane, or by
`comment --path`) print under their record in `show <card>`, threaded, each with
its word-id and `path:line` (`path:old a-b` for deleted lines). Answer one with
`nd headway comment <card> --reply-to <word-id> <text>`: the reply goes on the
same record under the same lines, not into the card's comment thread. It is
signed by the comment key like any comment (see below).

When the app sends review comments or an X send-back into your session, the
message ends with this same hint. Answer each comment where it was left, so the
reviewer sees the answer beside it, rather than only in your transcript:

```bash
nd headway show headway:headway/some-card     # each review comment prints with its word-id
nd headway comment headway:headway/some-card --reply-to ball-double-claw "done in abc1234"
nd headway comment headway:headway/some-card --path src/lib.rs --line 40-44 "kept the clone: X needs it"
```

A long comment goes through `--body-file -` with a heredoc, like `add
--desc-file -`, so nothing in it needs shell-escaping.

When commenting a finished card's commit hash, a Dave agentic session should also
quote its own `agentium:` session ref (from `$AGENTIUM_SESSION`) beside the hash —
see CLAUDE.md "When done with the work" and the `agentium` skill.

## Subissues

A card can be a **subissue** of one parent card (GitHub sub-issue semantics:
one parent per child, any number of children per parent). Use this instead of
a hand-maintained checklist in the description whenever work breaks down into
trackable pieces: the rollup is derived from the board, so it can never go
stale.

```bash
nd headway add "wire up the parser" --col todo --parent <epic>  # create as a subissue
nd headway parent <card> <epic>    # make an existing card a subissue (or re-parent)
nd headway parent <card>           # omit the parent to detach
```

Progress is **derived, not stored**: a child counts as done when it sits in a
**terminal** column of its board (see "Terminal columns" below — In Review and
Done on the default board), or is archived. There is no checkbox to tick —
moving the child card *is* the progress update.

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

- `nd headway next` prints the **ready frontier**: the cards workable *right
  now*, in work-order. `next` alone prints just the first (the single best next
  thing); `next --ready` prints the whole frontier; `next -n <k>` caps it at
  `k`; `-l <labels>` keeps only cards with those labels. More than one card can
  be ready at once — that's the parallel-dispatch signal (independent cards you
  could hand to different workers at the same time).
- `nd headway seq <card>`, with no position flag, prints where the card sits in
  that order and writes nothing: its place in the whole walk (`31 of 40`), the
  cards just before and after it, whether it has a rank of its own or is placed
  by creation time, how many *ready* cards `next` would hand out ahead of it,
  and why it isn't ready itself if it isn't (done, In Progress, blocked by a
  card, or has open subissues). The two counts differ on purpose: the place
  counts every card, "ready ahead" only the frontier. A card the container's
  walk never reaches is an error. `--json` gives `{ref, container, index, of,
  sequenced, before, after, ready_ahead, ready, not_ready_why}`. Use it rather
  than `next --ready | grep -n`, which drops blocked and In Progress cards.
- `nd headway seq <card> <pos>` positions a card in a container's work-order,
  which is how you *curate* the order `next` reads. `<pos>` is `--first`,
  `--last`, `--after <card>`, or `--before <card>`.

`--in <c>` names the container whose order you mean: a **card ref** targets that
card's subissues, a **board slug** targets the board root (its top-level cards);
omitted, it's the board root. `next` refuses to guess the board — pass
`--board <id>` or an `--in <headway:board/word-id>` ref.

```bash
nd headway next --board dave              # the single best next card
nd headway next --board dave --ready      # the whole ready frontier, in order
nd headway next --board dave -n 3         # the top 3
nd headway seq <card>                     # where <card> sits in the board-root order
nd headway seq <epic-child> --in <epic>   # where it sits within its epic
nd headway seq <card> --first --in dave   # make <card> the first board-root task
nd headway seq <epic-child> --after <sib> --in <epic>   # order within an epic
```

**What "ready" means.** A card is ready when it is *not done* (not sitting in a
**terminal** column — see "Terminal columns" below), *not In Progress* (someone
already has it), *not blocked* (no `block` edge pointing at an unfinished card —
see Dependencies), and *not a parent with unfinished subissues* (an epic's real
work is its children, which are in the frontier themselves, so the epic card
isn't dispatchable). On the default board both **In Review** and **Done** are
terminal, so a card in review reads as done and `next` will *not* resurface it.

**Ordering, and the priority caveat.** Within a container, members run in
`seq`-order where a `seq` has been set, else creation order — the board root
included. The walk is depth-first: each card is followed by its own subissues
before the next card. `next` does **not** sort by the `priority` field —
priority is a human-facing label, not an input to the frontier. So a board where
nobody has run `seq` has no real work-order: `next --ready` is just the board in
default order, and you should judge the biggest win yourself rather than trust the
first line. Curate with `seq` to make `next` meaningful.

## Searching cards (`grep`)

`nd headway grep <pattern>` finds the cards that *say* something, without a
`show --json` + `show <card>` loop: it searches every card's **title,
description, comment thread and inline review comments** in one run and prints
each matching line under the card's full `headway:<board>/<word-id>` ref and
title, with the field it came from and how long ago it was written (`2h ago`).
Cards are listed **most recent first**, by their newest matching line, across
every board searched. A comment is dated by itself; a title or description by
the edit that last set it. Labels and review records' commit subjects are not
searched. It's a read command — it never signs.

- `<pattern>` is a **regex**, **smart-case** by default: an all-lowercase
  pattern matches any case, one with an uppercase letter matches exactly. `-i`
  forces case-insensitive, `-s` case-sensitive. Quote a pattern with spaces —
  `grep` takes exactly one.
- Scope: **every board you can read** by default, your own and the ones shared
  with you. `--board <b>` searches just that board. `--in <card>` searches only
  that card **and everything under it** (the card itself is included, unlike
  `next --in`), on the board its ref names.
- **Archived cards are always searched**; their column shows as `archived`.
- On a terminal the matches are **colored and paged**: the pager is
  `$HEADWAY_PAGER`, else `$PAGER`, else `less -R`. Piped, output is plain and
  unpaged. `--no-pager` / `--pager` force paging off or on; `--color
  auto|always|never` overrides coloring. Agents reading the output can rely on
  the pipe, or pass `--no-pager` or `--json`.
- `--json` is an array with one object per matching card — `ref`, `id`,
  `board`, `title`, `column` (`archived` for an archived card) and `matches`
  (`[{field, text, created_at}]`, `field` one of `title`/`desc`/`comment`/`review`,
  `created_at` unix seconds), in the same most-recent-first order.
  "No matches" is `[]` (plain output says `no matches`); either way it exits 0.

```bash
nd headway grep 'relay reconnect' --board notedeck
nd headway grep -i nostrdb                           # every board you can read
nd headway grep sealed --in headway:notedeck/saddle-because-liquid   # one epic
nd headway grep 'Changelog' --json | jq -r '.[].ref'
```

## Terminal columns: what counts as "done"

Doneness — a card clearing its dependents, dropping out of the `next` frontier,
and rendering as done (strikethrough, `n/m` rollups) — is decided by which
**terminal** columns a board marks. A card in *any* terminal column is done.

- On the **default board**, both **In Review** and **Done** are terminal. So a
  card in review already counts as done: it stops blocking its dependents and
  `next` won't resurface it.
- A board that marks **no** terminal column falls back to treating its **last**
  column as terminal, so older boards are unchanged.
- Mark or clear the flag with `nd headway terminal <col> [on|off] --board <id>`,
  which republishes the board definition. Boards seeded before this feature
  carry no marks (last-column fallback) until you opt a column in — e.g. `nd
  headway terminal in-review on --board work`.

`show --json` reports each column's `terminal` flag. Terminal is about doneness
only; it doesn't merge or reorder columns — In Review and Done stay distinct
lanes you move cards through.

## Dependencies: `block` / `unblock` (and `relate`)

A card can be **blocked by** any number of other cards — a directed dependency
edge saying "don't start this until that is finished". It is a separate axis
from parent/subissue: a card can be both a subissue and blocked, and an edge may
point at a card on another board (name that card by its full hex id).

```bash
nd headway block <card> --on <blocker>      # <card> is blocked by <blocker>
nd headway block <card> <blocker>           # same — the blocker may be a 2nd positional
nd headway unblock <card> --on <blocker>    # drop the edge
```

Unlike `parent` (one parent, re-parenting replaces it), blockers **accumulate**:
each `block` adds to the card's set, and `unblock` removes one edge.

An edge edit that changes nothing says so, and never as a resolution failure —
so don't retry with a longer id. Re-adding an edge that's already there (`block`,
`relate`) or dropping one that isn't (`unblock`, `unrelate`) is idempotent
success: exit 0 with `ok (0 events) — <card> is already blocked on <blocker>`
(`--json` adds a `"noop"` field). A `block` or `parent` that would close a cycle
is a real error, and names what it refused.

A blocker is **cleared** the same way a subissue is done — when it sits in a
**terminal** column of its board (In Review or Done on the default board; see
"Terminal columns"), or is archived. There's nothing to tick: moving the blocker
*is* the unblock.

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
ordering and never affects blocking, the ready frontier, or any rollup. Card
detail shows it as a `related` section and `show --json` as a `related` array.

## Cross-board cards: `link` and `move-board`

Board membership is **placement-driven**: the same card — one issue with all
its overlays (title, description, labels, comments, parent/subissues) — can sit
on several boards at once. Two commands manage this:

```bash
nd headway link 1a2b3c4d… --board work --to personal        # now on both boards
nd headway move-board 1a2b3c4d… --board work --to personal  # re-homed: off work, on personal
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
  (or a ref naming it) with `--to <target>`. The target board must already
  exist — seed it first with `nd headway seed --board <target>` if it doesn't.

## Sharing a board

A sealed board can be shared with another nostr identity, who then reads and
edits it from their own notedeck:

```bash
# owner: gift-wrap the board's channel key to the member (kind-1082 in a 1059)
nd headway share npub1… --board ios-port
# member, in their own notedeck: its slug is enough
nd headway show --board ios-port
nd headway add "From a member" --board ios-port --col todo
```

- **Owner-only.** A member's `share` is refused; so is sharing to yourself.
- **Sealed boards only.** A plaintext board has no channel to hand out —
  `nd headway migrate --board <id>` it first (never on a real board casually).
- **`--board` is required**: a share can't be revoked, so the board must be
  named on purpose.
- Not idempotent on the wire (a fresh throwaway wrap key each run), but a
  repeat share is harmless: the member's roster dedups it.
- The member's edits fold on the owner's side attributed to the member's pubkey.

### Working on a board someone shared with you

- **`nd headway board` lists it** among the boards shared with you, with its
  owner, after your own boards. `show --all` includes it too (JSON boards carry
  an `owner` hex).
- **The slug resolves the owner.** A `--board <slug>` or a
  `headway:<slug>/<word-id>` card ref that isn't one of *your* boards folds the
  board shared with you under that slug, so a card ref pasted from the owner
  works as-is.
- **Your own board wins.** If you own a board with that slug, it is used
  instead; nd has no `--author` to reach the shared one past it.
- `seed` and `migrate` never resolve to someone else's board: they only act on
  boards you own.

## Commenting as an agent (a separate comment key)

An agent can sign its **comments** with its own key, so they show as the
agent's rather than the account's. Everything else (reads, adds, moves) stays
the account's:

```bash
HEADWAY_COMMENT_NSEC_FILE=~/.local/share/jex0/nsec \
  nd headway comment headway:headway/some-card "…"
# or inline: HEADWAY_COMMENT_NSEC=<nsec|hex>
```

`$HEADWAY_COMMENT_NSEC` wins over `$HEADWAY_COMMENT_NSEC_FILE`. The file holds
the key as nsec or hex (a trailing newline is fine); if it can't be read the
command fails rather than quietly signing as the account. nd reads the key on
the caller's side and hands it to the notedeck beside the call, never as a tool
argument. Dave sessions usually have `HEADWAY_COMMENT_NSEC_FILE` already set
through `session_env` in `dave_settings.json`, so a plain `nd headway comment`
signs as the agent.

The comment is sealed into the board's channel with the account's access, so
the comment key needs no membership and holds no board key. It only works on
**sealed** boards: a plaintext board shows just its owner's own events, so the
comment is refused rather than published where nobody would see it. On a
plaintext board, unset the variable for that one command
(`env -u HEADWAY_COMMENT_NSEC -u HEADWAY_COMMENT_NSEC_FILE nd headway comment …`)
to comment as the account.

## Typical workflow

Move a card from In Progress to Done:

```bash
nd headway show --board b --json                 # match the title, grab its hex `id`
nd headway move 1a2b3c4d… --board b --col done   # move by hex id (a column may match by name)
nd headway show 1a2b3c4d… --board b              # verify it landed in Done
```

Resolution errors are explicit: an ambiguous hex prefix says "ambiguous card
prefix", an unknown reference "no card matching", and a bad column lists the
valid column names — re-read `show` and retry with a corrected argument.

## Notes

- Edits print `ok (N events)`, `ok (0 events) — <why>` when nothing needed
  writing. An edit's reply waits until the write reads back, so a `show` right
  after it sees it.
- `seed` errors if a board already exists; that's expected — just `show` instead.
- A board seeded on one machine reaches the account's other machines through
  the running notedecks' relays. If a board is missing on one machine, give the
  notedecks a moment to sync and list again.
- A tool error goes to stderr with exit 1; no notedeck reachable is exit 2 (see
  the `nd` skill).
