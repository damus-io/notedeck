---
name: agentium
description: Query and control Dave agentic sessions from the command line via the `agentium` CLI (crates/agentium_cli), and find your OWN session's agentium: reference. Use when an agent running inside a Dave session needs its own ref (e.g. for a headway done-comment), to list sibling sessions, or to pull another session's detail or full conversation transcript — e.g. "what's my agentium ref", "list the running agent sessions", "show sessions in this cwd", "read session X's transcript".
---

# Agentium session CLI

`agentium` is a CLI over a running notedeck's embedded relay. It reads (and, in
time, controls) **Dave agentic sessions** — the Claude/Codex sessions Dave
spawns — by folding their kind-31988 session-state events. Like the `headway`
and `notebook` CLIs it keeps its own nostrdb cache, reconciles with the relay
each run (NIP-77 negentropy, falling back to NIP-01, or fully offline against
the cache), and reads your own sessions once you're logged in. Source:
`crates/agentium_cli`; session model in `crates/agentium-core`.

Sessions are **PNS-encrypted to their owner**, so `agentium` only ever lists
sessions decryptable by the signing key (an `--author` other than yours lists
nothing).

## Running it

Prefer a built binary; fall back to cargo:

```bash
# build once, then call the binary directly (fast, no rebuild per command)
cargo build -p agentium_cli           # produces target/debug/agentium
target/debug/agentium <command>

# or, one-off:
cargo run -q -p agentium_cli -- <command>
```

In examples below, `agentium` means whichever form you're using.

## Logging in

Everything operates on your own sessions once you're logged in. Relay defaults
to `ws://127.0.0.1:6677` (notedeck's embedded relay); override with
`--relay <url>` or `$AGENTIUM_RELAY`. A per-run key can be passed with `--nsec`
or `$AGENTIUM_NSEC`, but normally you run `agentium login <nsec>` once and it's
reused. If a command fails because you're not logged in, ask the user to run
`agentium login`. Don't handle the key yourself.

## The `agentium:` reference

Every session has a stable, sayable reference of the form
`agentium:<word-word-word>` (three BIP-39 words), e.g.
`agentium:maple-river-canyon`. It uses a `:` scheme (not a `#` sigil) so it
survives nostrdb tokenization and needs no shell quoting. The word-id is a
one-way hash of the session's stable id (its kind-31988 `d`-tag, the field
`claude_session_id` in JSON) — so you can derive the URI from the id but **not**
the id back from the words. Quote the `agentium:` URI when referring a human to a
session; keep the raw id when you need to match a session programmatically.

## Finding your OWN session ref

An agent running **inside** a Dave session (Claude or Codex backend) has its own
identity exported into its environment by `notedeck_dave` when the backend is
spawned:

- **`AGENTIUM_SESSION`** — the sayable `agentium:<word-id>` URI. This is the ref
  to quote into a headway done-comment or any human-facing note.
- **`AGENTIUM_SESSION_ID`** — the raw, lossless session id (the kind-31988
  `d`-tag). Use this when you need to match your row deterministically in
  `list --json`.

So the reliable, one-line way to get your own ref is just:

```bash
echo "$AGENTIUM_SESSION"        # -> agentium:maple-river-canyon
```

If `$AGENTIUM_SESSION` is unset (e.g. an older Dave build that predates the env
export), fall back to matching your raw id in the machine-readable listing:

```bash
# deterministic: match on the raw id
agentium list --json | jq -r \
  --arg id "$AGENTIUM_SESSION_ID" \
  '.[] | select(.claude_session_id == $id) | .agentium_uri'

# last resort with neither var set — filter by cwd/host and eyeball the row
# (ambiguous when several sessions share a cwd, so prefer the env vars):
agentium list --json --cwd "$PWD" --host "$(hostname)"
```

## `list` — read sessions

`list` prints this identity's sessions, newest first, grouped by host. Filters
are case-insensitive substring matches (except `--status`, which is exact):

```bash
agentium list                          # human-readable, grouped by host
agentium list --status working         # idle|working|needs_input|error|done|pending
agentium list --cwd notedeck           # sessions whose working dir contains "notedeck"
agentium list --host mbp --backend claude
agentium list --json                   # machine-readable; the raw session set
```

`--json` emits an array of objects, each the folded `SessionState` plus an added
`agentium_uri` field. Useful fields:

| field                | meaning                                                        |
|----------------------|----------------------------------------------------------------|
| `agentium_uri`       | the sayable `agentium:<word-id>` ref                           |
| `claude_session_id`  | the stable kind-31988 `d`-tag (matches `$AGENTIUM_SESSION_ID`) |
| `title`              | session title                                                  |
| `cwd`                | working directory                                              |
| `status`             | idle / working / needs_input / error / done / pending         |
| `hostname`           | host the session runs on                                       |
| `backend`            | claude / codex / …                                             |
| `cli_session_id`     | the backend CLI's own session id (for `--resume`); may be null |

Note the id distinction: `claude_session_id` is agentium's own stable identity
(what the word-id hashes), **not** the backend CLI's session id — that's
`cli_session_id`, a separate value used for resuming the underlying CLI.

## Selecting a session

`show`, `log`, `resume`, `send`, and `interrupt` take a **session selector** —
any of: the raw `claude_session_id` (d-tag), the `cli_session_id`, an
`agentium:<word-id>` ref (with or without the `agentium:` prefix), a unique id
prefix, or a unique title substring. For `show` and `log` the selector is
**optional** and defaults to `$AGENTIUM_SESSION`, so an agent running inside a
session can address itself (`resume`/`send`/`interrupt` always require an
explicit selector):

```bash
agentium show                    # this session (via $AGENTIUM_SESSION)
agentium log agentium:maple-river-canyon
agentium show "Fix relay reconnect"   # unique title substring
```

Selectors resolve across live **and** soft-deleted sessions, so a durable
`agentium:` ref still reads after its session was closed.

## `show` — session detail

`show` prints one session's detail: its kind-31988 state, the run-configs on its
host+cwd, its latest token usage, and a conversation summary (message count +
any pending permission request). `--json` emits the structured detail object.

```bash
agentium show                          # detail for the current session
agentium show maple-river-canyon --json
```

## `log` — conversation transcript

`log` prints one session's full conversation, **one entry per message, in
order** (millisecond wall-clock — the display order Dave itself uses). This is
the command for pulling context out of another session. Named after `git log`,
and like it, long output is paged.

```bash
agentium log                           # this session's transcript
agentium log maple-river-canyon        # another session's transcript
agentium log X --role user,assistant   # only the human turns + Dave's replies
agentium log X -n 20                    # just the last 20 messages
agentium log X --no-tools              # fold away tool_call/tool_result noise
agentium log X --json                  # structured, role-tagged message objects
agentium log X --jsonl                 # raw reconstructed claude-code JSONL
agentium log X --follow                # print the tail, then stream new messages
agentium log X -n 20 -f                # last 20, then follow (like `tail -n 20 -f`)
```

Flags:

| flag                       | effect                                                         |
|----------------------------|----------------------------------------------------------------|
| `--role <r[,r…]>`          | keep only these roles (comma-separated and/or repeatable); one of `user`, `assistant`, `tool_call`, `tool_result`, `permission_request`, `subagent`, `system`, `error`, `compaction`, `todo` |
| `--last <n>`, `-n <n>`     | only the last `n` messages (after other filters); with `--follow`, sizes the initial tail |
| `--tools` / `--no-tools`   | show (default) or fold `tool_call`/`tool_result` messages       |
| `--json`                   | one role-tagged JSON object per message (a lossy display view). With `--follow`, streamed newline-delimited (one object per new message) |
| `--jsonl`                  | raw reconstructed claude-code JSONL from the kind-1989 archive (the lossless source, in original `seq` order — a different axis than the display stream) |
| `--follow`, `-f`           | after the tail, keep streaming each new message as it lands (and status changes, e.g. `-> needs_input`) until Ctrl-C. Conflicts with `--pager`/`--jsonl` (a live stream can't be paged or reconstructed from the point-in-time archive) |
| `--color auto\|always\|never` | ANSI color; `auto` (default) follows the sink. Use `always` to keep color when piping into your own pager (e.g. `\| less -SR`) |
| `--pager` / `--no-pager`   | force/disable paging. Default: page when stdout is a terminal (never for `--follow`) |

The pager command is `$AGENTIUM_PAGER`, then `$PAGER`, else `less -R`. When
scripting (parsing output), prefer `--json`/`--jsonl`; piping to a non-terminal
already disables the pager and color automatically. `--follow` is `tail -f` for a
session's transcript — the streaming *mode* of `log`, not a separate command.

## `resume` — reopen a closed session

`resume <session>` reopens a closed (even soft-deleted) session on its host so a
new message drives its backend again, reviving its `agentium:` ref in place. It
needs a session whose backend actually started (a non-empty `cli_session_id`).

```bash
agentium resume maple-river-canyon
```

## `send` — send a message to a session

`send <session> <text…>` publishes a `user` message onto a **live** session's
conversation so its running agent (local *or* remote) picks it up over relay
sync, then reports the resulting event id. This is the first write command — the
way to steer another session from the shell.

```bash
agentium send maple-river-canyon run the tests and fix any failures
agentium send maple-river-canyon "keep the exact  spacing"   # quote to preserve it
agentium send maple-river-canyon hi --json                   # {"session","event_id"}
```

Notes:

- The selector is **required** — unlike `show`/`log` there is no
  `$AGENTIUM_SESSION` default, because the first word would be ambiguous against
  the message text. Pass an explicit selector (any form `list` accepts).
- The message is the remaining words **joined with single spaces**, so short
  prompts need no quoting; quote a single argument to preserve exact spacing.
  Empty/whitespace-only text is rejected.
- Only **live** sessions can be sent to. A soft-deleted session has no backend
  reading its conversation, so `send` refuses and points you at `resume` — reopen
  it first, then send.
- `--json` emits `{ "session": "agentium:…", "event_id": "<64-hex>" }`; the plain
  output is `sent to <agentium:ref> (event <hex8>…)`.

## `spawn` — create a new session on a host

`spawn` tells a (local or remote) Dave **host** to create a fresh session, then —
with `--wait` — blocks until that host answers with the new session's kind-31988
state and prints its durable `agentium:` ref. This is how you start a *new*
session you can then drive with `send`. The host must be a running `notedeck_dave`
on the target host; nothing answers otherwise (the wait times out cleanly).

```bash
agentium spawn                                   # sibling in this session's own host+cwd
agentium spawn --host mbp --cwd ~/dev/notedeck --wait   # print the new agentium: ref
agentium spawn --title "Fix the parser" --wait   # give it an explicit, sticky title
agentium spawn --host mbp --cwd ~/proj --prompt "run the tests" --wait
agentium spawn --host mbp --cwd ~/proj --wait --json    # {spawn_id,host,session,event_id}
agentium spawn --host mbp --cwd ~/proj --wait --wait-timeout 60   # give a slow host longer
agentium spawn --title "Fix the parser" --prompt-file - <<'EOF'   # heredoc a long first message
Fix the parser so it handles multi-line input.
See the failing test in crates/tokenator.
EOF
```

Flags:

- `--host <name>` / `--cwd <path>` / `--backend claude|codex` — the spawn target.
  **Omitted, they default to the current session's own state**
  (`$AGENTIUM_SESSION`), so a bare `agentium spawn` starts a sibling in the same
  worktree on the same host. `--backend` falls back to `claude`. If you're not
  inside a session, `--host`/`--cwd` are required.
- `--title <text>` — an explicit, **sticky** session title. Without it the title
  derives from (and churns with) the first message; `--title` lands in the
  session's `custom_title` so it shows immediately and survives later messages.
- `--wait` — block (bounded) until the host answers with the new session's state,
  then print its `agentium:` ref. Without it you only get the provisional
  `spawn_id` (the session doesn't exist yet). The bound defaults to 30s (a
  backgrounded Dave host answers on its own frame cadence, so a slow spawn can
  take ~20s); override it per run with `--wait-timeout <secs>` or
  `$AGENTIUM_SPAWN_WAIT`. After 8s a one-time `still waiting…` note prints on
  **stderr** (stdout/`--json` stays clean for scripting).
- `--wait-timeout <secs>` — override the `--wait` bound for this run (flag beats
  `$AGENTIUM_SPAWN_WAIT` beats the 30s default). Handy when a host is known slow
  (bump it) or you want a snappier failure against a host you expect to be up.
- `--prompt <text>` — deliver `<text>` as the session's first `user` message once
  it's up. **Implies `--wait`** (you can't send to a session that doesn't exist),
  and also reports the message event id. Equivalent to `spawn --wait` then `send`.
- `--prompt-file <path>` — the escaping-free alternative to `--prompt`: read the
  first message from a file, or from stdin when `<path>` is `-`, so a long
  multi-line prompt can heredoc in with no shell-escaping (the `/handoff` flow
  uses `--prompt-file -`). It resolves into the same first message as `--prompt`
  (so it likewise **implies `--wait`** and reports the message event id), trims
  trailing whitespace (so a heredoc's closing newline doesn't ride along), and is
  **mutually exclusive** with `--prompt`.

Output: plain, no `--wait` → `spawn command sent to <host> (spawn <id8>…)`; with
`--wait` → `spawned <agentium:ref> on <host> (spawn <id8>…)`, plus
`, sent prompt (event <hex8>…)` when `--prompt` seeded a message. `--json` emits
`{ "spawn_id", "host", "session", "event_id" }` — `session` is null until `--wait`
resolves it, `event_id` present only with `--prompt`. On a `--wait` timeout the
command was still published (a later `list` finds the session if the host was just
slow); the error names the `spawn_id`.

## `interrupt` — abort a session's in-flight turn

`interrupt <session>` aborts whatever turn/tool loop a **live** session is
running on its host — the CLI companion to pressing Esc in Dave. It publishes a
kind-1988 interrupt command the host applies to its backend (the same
`client.interrupt()` mechanism as the local Esc), then returns.

```bash
agentium interrupt maple-river-canyon
```

Notes:

- The selector is **required** (a specific session to interrupt); pass any form
  `list` accepts. There is no `$AGENTIUM_SESSION` default.
- Only **live** sessions can be interrupted. A soft-deleted session has no
  running backend, so `interrupt` refuses and points you at `resume`.
- Fire-and-forget: it reports `interrupt sent to <agentium:ref>`. The session
  drops back to idle once the host publishes its next state event after the turn
  aborts (watch it with `log --follow`).

## Command surface

Implemented: `list`, `show`, `log` (incl. `log --follow`, the live `tail -f`),
`resume`, `send`, `spawn`, `interrupt` (plus `login`/`logout`). Further control
verbs (watch dashboard, approve/deny, permission-mode, run-config management) are
planned but **not yet implemented** — don't invoke them until they land.
