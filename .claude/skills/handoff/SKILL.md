---
name: handoff
description: Hand off to a fresh Dave session — ask the user what to work on next, spawn a new session in the current worktree, and send it the task as its first message. Use when the user wants to delegate the next unit of work to a brand-new agentic session rather than continue in this one — e.g. "/handoff", "hand this off", "spawn a session to do X next", "start a fresh session on the next task".
---

# Handoff to a fresh Dave session

`/handoff` starts a **new** Dave agentic session in the *current worktree* and
seeds it with the next task, so this session can wrap up while a fresh one picks
up with clean context. It's a single `agentium spawn` call (see the `agentium`
skill): one command spawns a sibling on this host + cwd, waits for it to come up,
and delivers the task as its first message.

## 1. Decide the task

The new session inherits **zero context** from this conversation, so the task has
to be a self-contained prompt: what to build, the full `headway:board/word-id`
card ref if there is one, and any constraint the fresh agent can't infer from the
repo. Treat it like the prompt *you* were handed at the start of this session.
(The new session will read the card itself, so the prompt only needs the ref plus
a short TL;DR — not the card's full text.)

- If the user already gave the task in the `/handoff` invocation, expand it into
  that self-contained prompt.
- Otherwise **ask the user what the next session should work on**, then show them
  the exact prompt you're about to send and get their OK — it's the only thing
  the new agent will see.
- Also pick a **short title** (a few words) for the session, so it's identifiable
  in `agentium list` — e.g. the headway card's title or a terse summary of the
  task. Set it with `--title` rather than letting it derive from the first
  message.

## 2. Spawn + send, in one command

`agentium spawn` already defaults `--host`/`--cwd`/`--backend` to *this* session's
own state (`$AGENTIUM_SESSION`), so a bare spawn starts a sibling in the same
worktree on the same host — no need to look them up. `--prompt-file -` reads the
first message from stdin, so a heredoc carries a long, multi-line prompt with
**zero shell-escaping** (no wrestling with quotes, `$`, or newlines). `--wait`
blocks (bounded) until the host answers with the new session's kind-31988 state;
`.session` is its durable `agentium:` ref.

```bash
ref=$(agentium spawn --title "<short title>" --wait --json --prompt-file - <<'EOF' | jq -r .session
<the self-contained task prompt — as many lines as you like, no escaping>
EOF
)
echo "$ref"
```

That's the whole handoff. Notes:

- Use a **quoted** heredoc delimiter (`<<'EOF'`) so the shell doesn't expand `$`
  in the prompt body.
- To hand off into a **different worktree**, add `--cwd /path/to/worktree` (a Dave
  host must be running on the target host to pick up the spawn).
- If you're **not inside a Dave session** (`$AGENTIUM_SESSION` unset), there's
  nothing to inherit — pass `--host` and `--cwd` explicitly (fall back to
  `hostname` / `pwd`).
- `--prompt-file <path>` also takes a real file if you'd rather write the prompt
  to your scratchpad first; it's mutually exclusive with `--prompt`.

If `spawn --wait` times out, no Dave host is running on the target host (or the
cwd is wrong) — report that; nothing was created.

## 3. Report back

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
