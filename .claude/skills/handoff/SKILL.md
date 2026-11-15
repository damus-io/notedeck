---
name: handoff
description: Hand off to a fresh Dave session — ask the user what to work on next, spawn a new session in the current worktree, and send it the task as its first message. Use when the user wants to delegate the next unit of work to a brand-new agentic session rather than continue in this one — e.g. "/handoff", "hand this off", "spawn a session to do X next", "start a fresh session on the next task". `/handoff autowork <task>` also branches the new session off the `autowork` branch and has it land its commit there.
---

# Handoff to a fresh Dave session

`/handoff` starts a **new** Dave agentic session in the *current worktree* and
seeds it with the next task, so this session can wrap up while a fresh one picks
up with clean context. It's a single `nd agentium spawn` call (see the `agentium`
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
- Also pick the session's **title**, so it's identifiable in `nd agentium list`,
  and set it with `--title` rather than letting it derive from the first
  message. If the task is a **headway card, the title is the card's full ref**,
  `--title headway:<board>/<word-id>`: Dave's session list draws a ref title as
  the card's inline chip, so the row shows the card's live title and a click on
  it opens the card. Otherwise, a terse summary of the task (a few words).
- If the task is a **headway card**, link the session to it with
  `--issue-url headway:<board>/<word-id>` (the full ref, never a bare word-id —
  `spawn` rejects one). The card is then recorded on the session itself, so
  `nd agentium show` names it and `s` on the session in Dave opens it.
- If the invocation includes **`autowork`** (`/handoff autowork <task>`, or
  `--autowork`), the new session works on the `autowork` branch. Follow
  [*`autowork`: land on the `autowork` branch*](#autowork-land-on-the-autowork-branch)
  below. Don't put the word in the task prompt.
- If the new session should **investigate and get approval before writing code**,
  pass `--permission-mode plan`. Writing "start in plan mode" in the prompt does
  **not** work: the session's backend has already launched in the host's default
  mode by the time it reads that first message. The flag is the only thing that
  actually sets it.

## 2. Spawn + send, in one command

`nd agentium spawn` already defaults `--host`/`--cwd`/`--backend` to *this* session's
own state (`$AGENTIUM_SESSION`), so a bare spawn starts a sibling in the same
worktree on the same host — no need to look them up.

**Write the prompt to a file first, with the Write tool**, in your scratchpad
(e.g. `<scratchpad>/handoff-prompt.md`). Then spawn with `--prompt-file <path>`.
`--ref` waits (bounded) for the host's answer and prints only the new session's
`agentium:` ref, so a plain `$(...)` captures it:

```bash
ref=$(nd agentium spawn --successor --title "<title>" --prompt-file <scratchpad>/handoff-prompt.md --ref)
echo "$ref"
```

**Never put a heredoc inside `$(...)`**, and never compose the prompt in the
shell at all. macOS's `/bin/bash` is 3.2, which finds the end of a `$(...)` by
matching parens and doesn't skip heredoc bodies, quoted delimiter or not. The
first stray `)` in the prompt (a `1)` list item) or apostrophe (`don't`) ends the
substitution. The new session gets the prompt cut there, and the rest of the
prompt runs as shell: backticked commands in it really execute. A file read by
nd is never parsed by the shell.

When the task is a headway card, `<title>` is its ref and
`--issue-url headway:<board>/<word-id>` links it too; add
`--permission-mode plan` when the task should be investigated and approved before
any code is written.

`--successor` records the new session as carrying this one's work on: its state
names this session as `parent-session` with `relation successor`, which is how
Dave's list and scene draw the handoff.

## 3. Check the whole prompt arrived

`spawn` prints a receipt on stderr, `prompt: <n> bytes, <m> lines, last line:
"…"`. Its last line must be your file's last line. Then confirm the session got
it:

```bash
nd agentium log "$ref" --role user -n 1 --no-pager
```

The message it shows must end with your prompt's last line (the host delivers it
when the session comes up, so give a fresh session a few seconds). If the end is
missing, send the missing lines from the file. `$(sed …)` reading a file is safe;
only a heredoc *inside* `$(...)` is not:

```bash
nd agentium send "$ref" "$(sed -n '<first missing line number>,$p' <scratchpad>/handoff-prompt.md)"
```

That's the whole handoff. Notes:

- To hand off into a **different worktree**, add `--cwd /path/to/worktree` (a Dave
  host must be running on the target host to pick up the spawn).
- If you're **not inside a Dave session** (`$AGENTIUM_SESSION` unset), there's
  nothing to inherit — pass `--host` and `--cwd` explicitly (fall back to
  `hostname` / `pwd`).
- `--ref` fails, printing nothing on stdout, when the host doesn't answer in
  time. The spawn command and its prompt were still published, so run
  `nd agentium list` before spawning again; never re-run blind.
- If `--ref` is rejected as an unknown flag, the running notedeck predates it.
  Use `--wait --json` piped straight into `jq -r .session` instead (still with
  `--prompt-file <path>`, still no heredoc).

## `autowork`: land on the `autowork` branch

`/handoff autowork` gives the new session the same branch discipline as an `autowork`
chain lane: it starts from `autowork` and, when its commit is done, fast-forwards
`autowork` onto it. The mechanics live in the `autowork` skill's *Parallel chains*
section (`~/.claude/skills/autowork/SKILL.md`): `autowork` is never checked out, the
main worktree's branch belongs to the user, and the *Land* block moves `autowork` with a
compare-and-swap `update-ref`. Point at that block; don't copy it here or into the prompt.

**Before spawning**, in this session:

1. **Commit your own work first, and land it** with that *Land* block. The new session's
   first step is a rebase of this worktree. A dirty tree makes that rebase fail. A commit
   you make after the rebase reaches `autowork` only when the next session lands.
2. **Check that `autowork` is free**:
   `git worktree list | grep -q '\[autowork\]'` must find nothing. If the branch doesn't
   exist yet, the *Land* block creates it from the main worktree's branch.
3. **Check that the tree is clean**: `git status --porcelain` prints nothing. If it
   prints anything that isn't yours, someone else has work in this worktree. Hand off
   into another worktree with `--cwd`, or ask the user. Never stash it.

**Then append this paragraph to the task prompt**, after the task itself:

```
Branch: this handoff works on `autowork`. Before you change anything, rebase this
worktree's branch onto it (`git rebase autowork`); if that fails, stop and report. When
your commit passes the repo's checks, land it on `autowork` with the *Land* block in the
*Parallel chains* section of ~/.claude/skills/autowork/SKILL.md. Read that block from
the file; do not invoke the `autowork` skill, since this is not a chain. Quote the landed
hash in the card's done comment and pass it to `nd headway review --commit`.
```

The spawned session doesn't load any skill, so this paragraph is the only way it learns
about the branch. It names the block instead of restating it, so the rules stay in one
place.

## A `--wait` timeout does NOT mean nothing was created

`spawn` publishes the command **before** it starts waiting. So a timeout means
"we didn't see the host's answer in time" — not "nothing happened". A slow host
still materializes the session and still delivers the `--prompt`, because the
prompt rides the command.

**Never re-run the spawn on a timeout.** Check first:

```bash
nd agentium list --cwd "$(pwd)"
```

If the session is there, the handoff worked — take its ref and report it. Only if
nothing appeared is it worth suspecting no Dave host is running on the target host
(or that `--cwd` is wrong).

Re-running is also *guarded* now, not just discouraged: an identical spawn within
ten minutes is refused before publishing, naming the session it would have
duplicated, and a host that does see a repeat answers it with the session it
already made. `--allow-duplicate` overrides the guard when you genuinely want a
second session in the same worktree. Treat the guard as a backstop, not a licence
to retry blindly — check `list` first.

## 4. Report back

Tell the user the new session's ref and how to watch it:

    handed off to <ref> — follow it with `nd agentium log <ref> -f`

Then finish wrapping up *this* session per CLAUDE.md: commit, build and publish
the explainer (`visual-explainer` skill), `nd headway review <card> --explainer
<url>`, then the done comment with the hash, title and explainer URL, and move
the card. The handoff doesn't excuse leaving the current work half-done.

After an `autowork` handoff, your commit is already landed (step 1 above), and the new
session may already have moved this worktree's `HEAD`. So pass your landed hash to
`nd headway review <card> --commit <hash>` instead of relying on its `HEAD` default.

## Notes

- `nd agentium` runs in the notedeck on this machine and signs as its account,
  so there is no key to configure. If nd exits 2, no notedeck could be reached
  or started; say so rather than working around it.
- One handoff = one new session. To fan out several tasks, repeat `/handoff` (or
  spawn once per task); don't cram multiple unrelated tasks into one prompt.
