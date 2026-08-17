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

`show`, `log`, and `resume` take a **session selector** — any of: the raw
`claude_session_id` (d-tag), the `cli_session_id`, an `agentium:<word-id>` ref
(with or without the `agentium:` prefix), a unique id prefix, or a unique
title substring. For `show` and `log` the selector is **optional** and defaults
to `$AGENTIUM_SESSION`, so an agent running inside a session can address itself:

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
```

Flags:

| flag                       | effect                                                         |
|----------------------------|----------------------------------------------------------------|
| `--role <r[,r…]>`          | keep only these roles (comma-separated and/or repeatable); one of `user`, `assistant`, `tool_call`, `tool_result`, `permission_request`, `subagent`, `system`, `error`, `compaction`, `todo` |
| `--last <n>`, `-n <n>`     | only the last `n` messages (after other filters)               |
| `--tools` / `--no-tools`   | show (default) or fold `tool_call`/`tool_result` messages       |
| `--json`                   | one role-tagged JSON object per message (a lossy display view)  |
| `--jsonl`                  | raw reconstructed claude-code JSONL from the kind-1989 archive (the lossless source, in original `seq` order — a different axis than the display stream) |
| `--color auto\|always\|never` | ANSI color; `auto` (default) follows the sink. Use `always` to keep color when piping into your own pager (e.g. `\| less -SR`) |
| `--pager` / `--no-pager`   | force/disable paging. Default: page when stdout is a terminal   |

The pager command is `$AGENTIUM_PAGER`, then `$PAGER`, else `less -R`. When
scripting (parsing output), prefer `--json`/`--jsonl`; piping to a non-terminal
already disables the pager and color automatically.

## `resume` — reopen a closed session

`resume <session>` reopens a closed (even soft-deleted) session on its host so a
new message drives its backend again, reviving its `agentium:` ref in place. It
needs a session whose backend actually started (a non-empty `cli_session_id`).

```bash
agentium resume maple-river-canyon
```

## Command surface

Implemented: `list`, `show`, `log`, `resume` (plus `login`/`logout`). Further
control verbs (tail -f, watch dashboard, send, approve/deny, permission-mode,
spawn, run-config management) are planned but **not yet implemented** — don't
invoke them until they land.
