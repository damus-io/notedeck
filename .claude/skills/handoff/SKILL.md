---
name: handoff
description: Hand off to a fresh Dave session — ask the user what to work on next, spawn a new session in the current worktree, and send it the task as its first message. Use when the user wants to delegate the next unit of work to a brand-new agentic session rather than continue in this one — e.g. "/handoff", "hand this off", "spawn a session to do X next", "start a fresh session on the next task".
---

# Handoff to a fresh Dave session

`/handoff` starts a **new** Dave agentic session in the *current worktree* and
seeds it with the next task, so this session can wrap up while a fresh one picks
up with clean context. It's a thin sequence over the `agentium` CLI (see the
`agentium` skill): decide the task → spawn a sibling session on this host + cwd →
`agentium send` it the task.

## 1. Decide the task

The new session inherits **zero context** from this conversation, so the task has
to be a self-contained prompt: what to build, the full `headway:board/word-id`
card ref if there is one, and any constraint the fresh agent can't infer from the
repo. Treat it like the prompt *you* were handed at the start of this session.

- If the user already gave the task in the `/handoff` invocation, expand it into
  that self-contained prompt.
- Otherwise **ask the user what the next session should work on**, then show them
  the exact prompt you're about to send and get their OK — it's the only thing
  the new agent will see.
- Also pick a **short title** (a few words) for the session, so it's identifiable
  in `agentium list` — e.g. the headway card's title or a terse summary of the
  task. Set it with `--title` (below) rather than letting it derive from the
  first message.

## 2. Find this session's host + cwd (the current worktree)

Spawn onto the *same* host string the running Dave host listens on, and the
current working directory, by reading this session's own kind-31988 state:

```bash
host=$(agentium show --json | jq -r .session.hostname)
cwd=$(agentium show --json | jq -r .session.cwd)
backend=$(agentium show --json | jq -r '.session.backend // "claude"')
```

`agentium show` with no selector uses `$AGENTIUM_SESSION` (this session). If
you're not inside a Dave session (`$AGENTIUM_SESSION` unset), fall back to
`hostname` / `pwd` for those values — but a Dave host must be running on that host
to pick up the spawn. `cwd` is the current worktree root; if the user wants a
different worktree, use that path instead.

## 3. Spawn the session, then send it the task

Spawn a session and wait for the host to bring it up, capturing its new
`agentium:` ref, then hand it the task with `send`:

```bash
ref=$(agentium spawn --host "$host" --cwd "$cwd" --backend "$backend" --title "<short title>" --wait --json | jq -r .session)
agentium send "$ref" "<the self-contained task prompt>"
```

`--wait` blocks (bounded) until the host answers with the new session's
kind-31988 state; `.session` is its durable `agentium:` ref. `--title` sets a
sticky session title (otherwise it derives from — and churns with — the first
message). Then `agentium send` delivers the task as the session's first `user`
message, which its backend picks up over relay sync. `agentium spawn --host
"$host" --cwd "$cwd" --title "<title>" --prompt "<task>" --wait` does the
spawn-and-send in one call if you'd rather.

If `spawn --wait` times out, no Dave host is running on `$host` (or the cwd is
wrong) — report that; nothing was created.

## 4. Report back

Tell the user the new session's ref and how to watch it:

    handed off to <ref> — follow it with `agentium log <ref> -f`

Then finish wrapping up *this* session per CLAUDE.md (commit, headway comment +
move, etc.). The handoff doesn't excuse leaving the current work half-done.

## Notes

- `agentium` needs a signing key; inside a Dave session it's already configured.
  If a command complains you're not logged in, see the `agentium` skill's
  "Logging in" section.
- One handoff = one new session. To fan out several tasks, repeat `/handoff` (or
  spawn once per task); don't cram multiple unrelated tasks into one prompt.
