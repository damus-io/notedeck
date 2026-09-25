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

`show`, `log`, `resume`, `send`, `interrupt`, `approve`/`deny`, and `mode` take
a **session selector** —
any of: the raw `claude_session_id` (d-tag), the `cli_session_id`, an
`agentium:<word-id>` ref (with or without the `agentium:` prefix), a unique id
prefix, or a unique title substring. For `show` and `log` the selector is
**optional** and defaults to `$AGENTIUM_SESSION`, so an agent running inside a
session can address itself (the commands that act on a session always require
an explicit selector):

```bash
agentium show                    # this session (via $AGENTIUM_SESSION)
agentium log agentium:maple-river-canyon
agentium show "Fix relay reconnect"   # unique title substring
```

Selectors resolve across live **and** soft-deleted sessions, so a durable
`agentium:` ref still reads after its session was closed.

## Reading without a relay round-trip

`--no-sync` skips the relay reconcile and reads the local cache as it stands.
The reconcile is what a read actually spends its time on, so this is the fast
path when you're querying a corpus that hasn't moved — e.g. several `grep`s in a
row, or a `log` you already synced a moment ago:

```bash
agentium grep -i wgpu --no-sync        # no relay round-trip at all
```

It applies to the read commands (`list`, `show`, `log`, `grep`, `config
list`/`show`) and is refused for anything that publishes or streams (`send`,
`spawn`, `resume`, `interrupt`, `approve`/`deny`, `mode`, `config
add`/`edit`/`rm`, `log --follow`), which can't work offline.

## `show` — session detail

`show` prints one session's detail: its kind-31988 state, the run-configs on its
host+cwd, its latest token usage, and a conversation summary (message count +
every pending permission request, each with the 8-digit id `approve`/`deny
--request` takes). `--json` emits the structured detail object, with pending
requests under `conversation.pending_permissions`.

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
agentium log X --tools                 # include tool_call/tool_result (folded by default)
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
| `--tools` / `--no-tools`   | show or fold (default) `tool_call`/`tool_result` messages. A transcript is mostly tool noise, so `log` reads as the human conversation until `--tools` asks for the rest |
| `--json`                   | one role-tagged JSON object per message (a lossy display view). With `--follow`, streamed newline-delimited (one object per new message) |
| `--jsonl`                  | raw reconstructed claude-code JSONL from the kind-1989 archive (the lossless source, in original `seq` order — a different axis than the display stream) |
| `--follow`, `-f`           | after the tail, keep streaming each new message as it lands (and status changes, e.g. `-> needs_input`) until Ctrl-C. Conflicts with `--pager`/`--jsonl` (a live stream can't be paged or reconstructed from the point-in-time archive) |
| `--color auto\|always\|never` | ANSI color; `auto` (default) follows the sink. Use `always` to keep color when piping into your own pager (e.g. `\| less -SR`) |
| `--pager` / `--no-pager`   | force/disable paging. Default: page when stdout is a terminal (never for `--follow`) |

The pager command is `$AGENTIUM_PAGER`, then `$PAGER`, else `less -R`. When
scripting (parsing output), prefer `--json`/`--jsonl`; piping to a non-terminal
already disables the pager and color automatically. `--follow` is `tail -f` for a
session's transcript — the streaming *mode* of `log`, not a separate command.

## `grep` — search message text across sessions

`grep <pattern>` searches every session the `list` filters select and prints each
matching line under its session's `agentium:` ref. This is the command for
"which session was it where I…" — the one that answers across the whole corpus
instead of one transcript.

```bash
agentium grep "resize hook"                       # every live session
agentium grep -i terminal --cwd Shipwright --all  # case-insensitive, one tree, incl. deleted
agentium grep '\bTODO\b' --role assistant         # a regex, over Dave's replies only
agentium grep -i panic --tools                    # search tool call/result text too
agentium grep -i wgpu --json                      # matches grouped under each session
```

The pattern is a **regex** (Rust `regex` syntax); `-i`/`--ignore-case` folds case.
An unparseable pattern fails immediately, before any relay work.

Which sessions are searched: the same filters `list` takes — `--cwd`, `--host`,
`--status`, `--backend`, and `--deleted`/`--all` for tombstoned sessions.
Which *messages* are searched: the same filters `log` takes — `--role`,
`--tools`, `--last`. So `--role assistant` searches only Dave's replies, and
`--cwd foo --all` searches every session (live or closed) in that tree. Tool
traffic is folded by default here too, which matters more than in `log`: a
`tool_result`'s searched text is its one-line render summary, never its full
output, so tool hits are mostly the command line that happened to mention the
word. Pass `--tools` when you do want those.

Matching is per line, like `grep`, over the same rendered body `log` prints, and
`--json` emits one object per matching session: the session's `list --json`
fields (so `agentium_uri` feeds straight back into `log`/`send`) plus a `matches`
array of `{role, text}`.

**Prefer this over a shell loop.** The obvious hand-rolled equivalent —

```bash
for s in $(agentium list --json | jq -r '.[].agentium_uri'); do agentium log "$s" | grep -i term; done
```

— re-opens the cache and re-reconciles the relay once per session, so it costs
seconds *per session*. `grep` syncs once and reads every session from one
transaction. Measured on an 879-session corpus: 3m40s for the loop, 36s for
`agentium grep --all` (and under a second over the live sessions alone).

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
on the target host; with none running, nothing answers and the wait times out —
but note the command is published *before* the wait, so a timeout is not proof
nothing was created (see **Retrying a spawn is safe** below).

```bash
agentium spawn                                   # sibling in this session's own host+cwd
agentium spawn --host mbp --cwd ~/dev/notedeck --wait   # print the new agentium: ref
agentium spawn --title "Fix the parser" --wait   # give it an explicit, sticky title
agentium spawn --host mbp --cwd ~/proj --prompt "run the tests" --wait
agentium spawn --permission-mode plan --prompt "design the migration" --wait  # plan first
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
- `--permission-mode <mode>` — the permission mode the new session's agent
  **starts in**, instead of the host's default: `default` (aka `manual`) | `plan`
  | `accept_edits` | `auto` | `bypass`. Aliases are normalized, so `acceptEdits`
  and `accept-edits` both work; an unknown mode is rejected before anything is
  published. Use `plan` when the new session should investigate and get approval
  before writing code. **Asking for a mode in the prompt does not work** — the
  session's backend has already launched by the time it reads its first message,
  so this flag is the only way to choose the starting mode. (`bypass` does no
  safety checking at all; it exists here because a spawn is the one moment a
  backend's mode is chosen, but reach for it deliberately.) To change the mode of
  an *already running* session, use dave's Ctrl+M / mode badge — there is no CLI
  verb for that yet.
- `--prompt-file <path>` — the escaping-free alternative to `--prompt`: read the
  first message from a file, or from stdin when `<path>` is `-`, so a long
  multi-line prompt can heredoc in with no shell-escaping (the `/handoff` flow
  uses `--prompt-file -`). It resolves into the same first message as `--prompt`
  (so it likewise **implies `--wait`** and reports the message event id), trims
  trailing whitespace (so a heredoc's closing newline doesn't ride along), and is
  **mutually exclusive** with `--prompt`.
- `--idempotency-key <k>` / `--allow-duplicate` — duplicate control; see
  **Retrying a spawn is safe** below. You rarely pass either: the key is derived
  for you, and `--allow-duplicate` is only for deliberately starting a second
  session the guard would refuse.

Output: plain, no `--wait` → `spawn command sent to <host> (spawn <id8>…)`; with
`--wait` → `spawned <agentium:ref> on <host> (spawn <id8>…)`, plus
`, sent prompt (event <hex8>…)` when `--prompt` seeded a message. `--json` emits
`{ "spawn_id", "host", "session", "event_id" }` **on one line** — `session` is null
until `--wait` resolves it, `event_id` present only with `--prompt`. On a `--wait`
timeout the command was still published, so the session may well exist; the error
names the `spawn_id`. See **Retrying a spawn is safe** below before re-running.

### Retrying a spawn is safe — but check `list` first

`spawn` publishes the command **before** it starts waiting, so a `--wait` timeout
means "we didn't see the answer in time", **not** "nothing was created". A slow
host still materializes the session, and still delivers a `--prompt` (which rides
the command). Re-running on a timeout is how you end up with two agents in one
worktree.

So on a timeout, look before you leap:

```bash
agentium list --cwd /path/to/worktree      # did the session actually appear?
```

Two defences also stand behind you:

- A spawn that repeats a recent one — same host, cwd and `--title`, within ten
  minutes — is **refused before publishing**, naming the session it would have
  duplicated plus how to follow it. Untitled spawns aren't guarded (there is
  nothing to compare), so they lean on the second defence.
- Every spawn carries an `idempotency_key` derived from the request itself
  (host+cwd+backend+title+prompt+mode). A host that has already materialized a
  session for that key **answers the repeat with that session** instead of
  creating another, and does not re-deliver its prompt. Pass
  `--idempotency-key <k>` to name the request yourself when you have a better
  notion of identity (a job id, say).

`--allow-duplicate` opts out of both — use it when you really do want a second
session in the same worktree. Neither defence helps against an old `agentium`
binary or an old Dave host, so checking `list` stays the habit.

**Don't add `tail -1`/`head -1`/`read -r` to a `--json` pipeline.** `spawn --json`
and `send --json` each emit one record on one line, so `| jq -r .session` is all
you need; against an older build that pretty-printed, those filters turn a
*successful* spawn into an empty string — which reads as a failure and invites
exactly the retry described above.

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

## `approve` / `deny` — answer a pending permission request

`approve <session>` / `deny <session>` answer a **live** session's pending
permission request — the CLI companion to the allow/deny buttons in Dave. By
default they answer the **newest** pending request; `show` lists every pending
one with an 8-digit id, and `--request <id-prefix>` picks another.

```bash
agentium show maple-river-canyon            # "pending permissions" lists ids
agentium approve maple-river-canyon
agentium deny maple-river-canyon --request 3f2a9c1e --message "use rg instead"
agentium deny maple-river-canyon --interrupt   # deny AND stop the turn
```

Notes:

- `--message <text>` rides with the decision and the agent sees it (a deny
  reason, or a note with an approve). `--interrupt` is deny-only.
- An `AskUserQuestion` request can be denied but **not approved** — approving
  one means sending its answers, which a bare approve can't. Answer it in Dave.
- Nothing pending (or a `--request` prefix that matches none, or several) is an
  error, listing what is pending.
- **No ack.** The host matches the answer against the requests it holds in
  memory. If it restarted since the request was made, the answer is silently
  ignored and the request keeps showing as pending.
- `--json` prints one line: `{session, event_id, perm_id, tool_name, decision,
  interrupt}`.

## `mode` — change a session's permission mode

`mode <session> <mode>` switches a **live** session's permission mode on its
host — the CLI companion to Ctrl+M in Dave. Modes: `default` (aka `manual`) |
`plan` | `accept_edits` | `auto` | `bypass`; aliases like `acceptEdits` are
normalized before publishing, and an unknown mode is refused (the host would
otherwise quietly fall back to `default`).

```bash
agentium mode maple-river-canyon plan
```

## `config` — manage run configs

A run config is what Dave's per-session run bar launches: a named shell command
(`cargo run`, say) registered for one host + working directory, run there with
`sh -c`. It is **not** a session template — `spawn` doesn't take one.

```bash
agentium config                      # = config list: every host, grouped host → cwd
agentium config list --host macbook  # --host/--cwd narrow (substring)
agentium config show 1a2b3c4d        # by id prefix (the 8-char id list prints) or exact name
agentium config add --name build --command "cargo build"   # on this session's host+cwd
agentium config add --name serve --command "npm start" --host macbook --cwd /home/u/app
agentium config edit build --command "cargo build --release"   # same id, new revision
agentium config rm build
```

- `add` places the config on `--host`/`--cwd` (exact values here, not
  substrings), each defaulting to the current session's (`$AGENTIUM_SESSION`);
  with neither it errors. `--name`/`--command` are trimmed and must be
  non-empty.
- A selector matching more than one config (a name used on two hosts, say)
  errors with the candidates; narrow with `--host`/`--cwd` or pass more id.
- Dave shows a config only on its own host, in a session whose cwd is exactly
  the config's. `rm` of a config that is running there kills its process.
- `--json`: `list` is a flat array of `{id, name, command, host, cwd,
  updated_at}`, `show` one such object; `add`/`edit`/`rm` print one line
  `{action, event_id, config}`.

## Command surface

Implemented: `list`, `show`, `log` (incl. `log --follow`, the live `tail -f`),
`resume`, `send`, `spawn`, `interrupt`, `approve`/`deny`, `mode`, `config`
(plus `login`/`logout`). A watch dashboard is planned but **not yet
implemented** — don't invoke it until it lands.
