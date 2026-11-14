---
name: nd
description: Drive the running notedeck from the command line via the `nd` CLI (crates/nd_cli) — list its tools and call any of them over its local socket, or spawn a Dave session, e.g. open a headway card or agentium session in the window, open the review queue, or read board data. Use when the user wants to open something in notedeck from a shell, script the app, or see which tools the running notedeck offers — e.g. "open this card in notedeck", "what tools does notedeck have", "call open".
---

# nd: notedeck's tools from a shell

`nd` talks to the running notedeck over its per-user socket
(`notedeck_ipc::socket_path()`, overridden by `$NOTEDECK_SOCKET`). Every run
fetches the tool list from the app, so it always matches what's running and
there's nothing to keep in sync. Source: `crates/nd_cli`.

## Running it

```bash
cargo run -q -p nd_cli -- tools          # or the built target/debug/nd
scripts/install-clis                     # nd + notedeck into ~/.cargo/bin
scripts/install-clis --root ~            # ...or into ~/bin
```

`scripts/install-clis` installs both binaries in release, because nd starts
`notedeck` itself when none is running (below), so both have to be on PATH.

## When no notedeck is running

nd starts one: `notedeck --headless --serve` (`$NOTEDECK_BIN`, else `notedeck`
on PATH), detached in its own session, and waits up to 15s for it to list its
tools. That notedeck runs the data apps only (Headway, Notebook, Dave), and its
Dave is **not a host**: it never takes a kind-31989 spawn or runs an agent. It
binds the socket only if no notedeck answers there, exits once another
notedeck takes the socket (opening the GUI wins), and exits after 10 minutes
without a request (`--idle-exit <secs>`). It holds no local relay. Its
stdout/stderr go beside the socket as `notedeck.log`; its own log is in the
data dir as always. `nd --no-start …` or `ND_NO_START=1` keeps the old exit 2
instead.

## Commands

- `nd tools [--json] [<pattern>]` — every tool, grouped by category, marked
  `data`/`ui` and `ro` (read-only). A pattern filters by name, title or
  description (smart-case regex).
- `nd tools <name>` — one tool's description and its arguments.
- `nd call <name> [key=value…]` — call a tool. Values are coerced by the
  tool's schema (string, number, true/false, JSON for object/array). A wrong or
  missing key fails before anything is sent, listing the arguments. A tool
  whose only required argument is a string takes it bare:
  `nd call open agentium:<word-id>`.
  `--json '<object>'` or `--json-file <path|->` passes the arguments as one
  object instead.
- `nd <app> <command> [args…]` — a tool that declares a command, e.g.
  `nd headway show <card>`, `nd headway next --in <epic> -n 1`, and the card
  edits `nd headway add <title…> --col todo -l a,b`, `move <card> --col
  in-review`, `comment <card> <text…>` (`--reply-to`, `--path`/`--line`,
  `--body-file -`), `title`, `desc`, `label`, `priority`, `due`, `estimate`,
  `archive`, `restore`, `delete`, spelled as the `headway` CLI spells them.
  A card ref names its board; else `--board`, else `$HEADWAY_BOARD`.
  `nd <app> --help` lists them. It prints the text the tool renders (what the
  app's own CLI prints, colored and paged through `$<APP>_PAGER`/`$PAGER` on a
  terminal); `--json` prints the value instead, the same JSON as the CLI's
  `--json`. `--pager`/`--no-pager` and `--color <auto|always|never>` go with
  every command.
- `nd mcp [--ui] [--read-only]` — a stdio MCP server: `tools/list` and
  `tools/call` go to the socket, one connection each, so it can start before
  notedeck. Only data tools are exposed unless `--ui`; `--read-only` keeps only
  tools that change nothing. A hidden tool can't be called by name. Register
  it with `claude mcp add notedeck -- nd mcp`.
- `nd spawn [--json] [<dir>]` — open a Dave session in `<dir>` (default the
  current directory, resolved and checked locally) and focus it, starting Dave
  if it isn't open. Prints `Spawned session <id>`, or the bare id with
  `--json`. It calls `dave_spawn_session`; `agentium spawn` is the relay one
  for other hosts.
- `nd --version` — nd's version, and the running notedeck's.
- `nd <cmd> --help`, `nd help options` — per-command pages, exit codes.

## Examples

```bash
nd call open headway:notedeck/balcony-warm-action
nd call open agentium:evolve-rule-carry
nd call open reference=agentium:evolve-rule-carry msg="rebase onto master"
nd call headway_open_review_queue
nd call headway_show_card --json '{"card":"headway:notedeck/balcony-warm-action"}'
```

## Output and exit codes

- `nd call` data prints as JSON on stdout (indented on a terminal, one line in
  a pipe), exit 0. An app command prints its tool's text, or that JSON with
  `--json` or when the tool renders none.
- A UI tool that acted on the window prints nothing, exit 0.
- A tool error goes to stderr, exit 1 (also for a bad command line).
- No notedeck listening and none could be started, or `--no-start`:
  `notedeck isn't running (no socket at …), and <why>`, exit 2.

A headless notedeck (`--headless`) refuses UI tools whose answer is a window
action. Tests must point `NOTEDECK_SOCKET` at a short temp path (a Unix socket
path must fit in ~100 bytes), never the user's real socket, and set
`ND_NO_START=1` (or a fake `NOTEDECK_BIN`) so a test never starts the real
notedeck.
