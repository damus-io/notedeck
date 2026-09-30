# Review queue

When an agent finishes a card, it records which commit did the work, and where.
Headway uses that record to show the commit's diff in the app. If the commit
isn't on this machine, headway fetches it first. The **review queue** (board
key `R`) steps through every In Review card this way: diff, explainer,
verdict. The epic is `headway:headway/mom-charge-attack`.

## The review record: kind `1626`

A regular, append-only event, so one card can collect a record per commit and
per host. Newest first, it folds into `CardView::reviews`. The full shape is in
the [README](../README.md#review-record--kind-1626-custom-append-only). In
short: content is empty, `e` names the card, and every other tag is optional.

| Tag         | Value                                                     |
| ----------- | --------------------------------------------------------- |
| `commit`    | full sha                                                  |
| `title`     | commit subject                                            |
| `branch`    | branch it was made on                                     |
| `host`      | hostname of the machine that recorded it                  |
| `path`      | repo toplevel on that host                                |
| `repo`      | repo identity: the root commit (smallest, if several)     |
| `agentium`  | `agentium:<word-id>` of the session that did the work     |
| `explainer` | URL of the explainer page                                 |
| `remote`    | explicit fetch URL, overriding the host-derived one       |

The tag names are spelled in one place, `ReviewFields::tags`
(`crates/headway/src/event/model.rs`). Sealed boards wrap the record like any
other kind. Re-recording the same fields (a retried done step) collapses to
one row.

## CLI

```sh
# Record HEAD on the card (sha, subject, branch, host, toplevel and root
# commit all come from git; the session from $AGENTIUM_SESSION).
headway review headway:headway/<word-id> --explainer https://claude.ai/artifact/…

# Print the newest record's commit git-show style, fetching it if needed.
# Where it was found goes to stderr, so stdout pipes into `git apply`.
headway diff headway:headway/<word-id> [--record <sha-prefix>]
```

The `autowork` skill and the repo's Work Tracking run `headway review` as the
first step of closing a card.

## Resolution: finding the commit on this host

`headway::git::resolve` (`crates/headway/src/git/resolve.rs`) is shared by the
CLI and the pane. It is blocking, so the pane runs it off the UI thread.

1. **Pick a target repo.** The first match wins:
   1. the record's `path`, if the record was made on this host;
   2. a local checkout of the same repo, matched by `repo`. Candidates are the
      CLI's own checkout, plus any `path` that a record on this board says was
      recorded on this host;
   3. a headway-owned **bare cache**, `<cache>/<repo>.git`. The GUI keeps it
      under the notedeck data dir at `cache/headway/git/`. The CLI keeps it
      under its config dir, or `<--db>/git`.
2. **Already there?** Done ("found locally in …").
3. **Fetch it.** The source is the first of: the record's `remote`, the
   recorded `path` (for a record made on this host), a remote of the target
   repo whose URL host is the record's `host` (a `jex0` remote serves a `jex0`
   record), or `<host>:<path>` over ssh. The fetch runs
   `git fetch --no-tags <src> <sha>` and falls back to fetching `branch` when
   the server won't serve a bare sha. It creates no refs, so a user checkout
   only gains a `FETCH_HEAD`, and the cache pins what it resolved.
4. **Trailer fallback.** If the sha still can't be found (usually a rebase),
   look for the newest commit whose `Headway: <card ref>` trailer names the
   card. The pane marks such a commit "hash differs from the record?". A card
   in a terminal column with no record at all is looked up this way too.

## Queue keys

`R` on the board opens the queue over this board's In Review column (the column
with id `in-review`, else one named In Review). The card list is fixed when the
queue opens, and the whole walk is one global-nav history entry.

`R` on a card's detail opens the queue over **that card's subtree** instead:
every sub-issue, at any depth, sitting in In Review, in the card's work-order
(`headway::traversal::work_order`, the order `headway next` and autowork walk
it), so an epic's queue replays its chain card by card. The Sub-issues header
offers the same as a "Review N" button when N of them are in review. The
queue's header names the epic (`in <word-id>`, its title on hover) beside the
position; with nothing in review under the card it says "Nothing in review
under this card" and opens nothing, rather than falling back to the board's
queue. Its history entry carries the epic
(`HeadwayRoute::ReviewQueue { epic }`), so back lands on the epic's detail and
forward reopens the same epic's queue where it was left. Back or forward onto
a queue entry of the other scope (the board's after an epic's, or the reverse)
retakes that scope's snapshot from the board as it is now
(`BoardUiState::refresh_queue`); with nothing left in review there, the entry
backs off onto the view under it and says so.

The queue takes the same keys as every other view, from one table: the card
actions (`Enter`/`o`, `e`, `s`/`S`, `r`, `a`, `D`, `X`, `n`/`p`) and a review
pane's navigation (`j`/`k`, `Space`/`Shift-Space` and `Ctrl-f`/`Ctrl-b` a
page, `Ctrl-d`/`Ctrl-u` half a page, `gg`/`G`, `]`/`[`). See
[Keys](../README.md#keys) in the README. What is the queue's own:

- `n` / `p` step through the queue's cards, stopping at the ends.
- A verdict steps it on: `D` (Done), `X` (reason, then In Progress) and `a`
  (archive). A verdict on the last card closes the queue ("Review queue done").
- `r` goes back to the card's newest record; the queue stays on the card.
- The queue's cards are a snapshot taken when it opened. A card that has left
  the board since (archived elsewhere) takes no card action but `n`/`p`; the
  rest say "This card has left the board".
- `q` / `Esc` leave for the grid, with the cursor on the card last shown; an
  epic's queue leaves for the epic's detail. So does a verdict on its last
  card. An epic that has left the board meanwhile has no detail to land on,
  so its queue leaves for the grid, as the board's does.

A review pane opened from a card's "Review diff" instead of the queue takes the
same keys; there `n`/`p` open the neighbouring card's review in its column and
`q`/`Esc` back out to the card's detail. The pane sits on its card's detail in
the global history however it was opened: the grid's `r` and a pane's `n`/`p`
push the detail's entry under the review's in the same frame, so `q` (one
back) always lands on the card shown. `a` backs out to that detail, which
leaves for whatever is under it once the archive lands.

The keymap is `keys::review_pane_keys` (both modes, `PaneMode`), and its strip
is `keys::QUEUE_STRIP` / `keys::PANE_STRIP`. The tests
`every_queue_hint_does_what_it_says` and `every_review_pane_hint_does_what_it_says`
check each strip entry against the keymap.

## When a fetch fails

The pane shows the git command it ran and git's stderr word for word, with a
**Retry** button. A failed load also retries on its own after 30 seconds. The
fetch never prompts: `GIT_TERMINAL_PROMPT=0`, and ssh runs with
`BatchMode=yes -o ConnectTimeout=10` unless you've set `GIT_SSH_COMMAND`. A
fetch is killed after 60 seconds. So:

- **`Permission denied` / `Host key verification failed`**: ssh needs a
  password or an unknown host key. Run `ssh <host>` once by hand, or load the
  key into your agent. Headway won't answer ssh's prompt for you.
- **`Could not resolve hostname`**: the recorded `host` isn't reachable under
  that name from here. Add a git remote in your checkout whose URL uses a
  name you *can* reach and whose host part matches the record (step 3), or
  record with `headway review --remote <url>` next time.
- **`not our ref` / `unadvertised object`**: the server won't serve a bare
  sha, and the branch fallback didn't bring it in either (the branch moved or
  was deleted). If the commit was rebased, the trailer search usually finds it
  once the new commit is fetched.
- **Nothing to fetch from**: the record has neither `remote` nor `host`/`path`.
  Check out the repo anywhere on this host and record a card from it, so its
  path becomes a known checkout.

## Tests

- `review_queue_shows_the_recorded_commit_diff` (plain `cargo test`) and
  `snapshot_headway_review_queue` (lavapipe, `scripts/snapshot-test`) share one
  fixture: a three-commit repo at the fixed path
  `/tmp/headway-review-fixture/notedeck`. The path is fixed because the pane
  prints it. The queue's records come from host `jex0`, and a record on the Done
  card says this host has a checkout of the repo, so the pane finds the commit
  locally and never fetches.
- `review_queue_walks_the_in_review_column`, `chrome_nav_loop_review_queue_is_one_entry`
  and the `keys.rs` unit tests cover stepping, history and verdicts.
