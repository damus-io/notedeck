# Notedeck - Claude Code Guidelines

## Work Tracking

All work is tracked on the **Headway board** via the `headway` CLI, not GitHub
Issues. See the `headway` skill (`.claude/skills/headway/SKILL.md`) for the full
command reference; the board flows
`Backlog → Todo → In Progress → In Review → Done`.

- **Before starting work**: `headway show` to read the board. If a card for the
  work exists, move it to In Progress: `headway move <card> --col in-progress`.
  If none exists, add one: `headway add "<title>" --col in-progress`.
- **Task breakdown**: Use one card per unit of work; `desc`/`label` for detail.
- **When done with the work**: Always commit your changes (see Committing
  below), then comment on the card with the commit hash so future iterations can
  see what's already been done: `headway comment <card> "committed <hash>: ..."`.
  This comment is read in the context of code review and follow-up work, so use
  it to note anything specific that should be tested or interesting things worth
  flagging beyond the commit message that would help someone reviewing the
  implementation. Then move the card to In Review so the change can be tested:
  `headway move <card> --col in-review`. Don't leave finished work uncommitted
  or sitting in In Progress. Leave the card in In Review until verified.
- **On completion**: Once the change is verified, move the card to Done:
  `headway move <card> --col done`.
- Cards are addressed by a short id prefix (from `show`); always `show` before
  editing. If a command fails because you're not logged in, ask the user to run
  `headway login`.

## Committing

Before committing, run the local CI checks:

    ./scripts/ci-local

This runs changelog trailer checks, lint (fmt + clippy), tests, and the android build — all parsed directly from the GitHub workflow YAML. You can also run individual jobs via `./scripts/ci.py`, e.g. `./scripts/ci.py lint`.

After a change lands, move its Headway card to In Review
(`headway move <card> --col in-review`); move it to Done once it's verified.

Every commit must include a Changelog git trailer. For user-facing changes, use `Changelog-{Added,Changed,Fixed,Removed}`. For internal changes (refactors, CI, tooling, docs, etc), use `Changelog-None:`. Examples:

   Changelog-Added: Add new zap metadata stats on notes

Bug fixes

   Changelog-Fixed: Fix a bug with foo's not toggling the bars

Or if there's nothing interesting to note (refactors, etc)

   Changelog-None:

When fixing a bug introduced by another commit, add:

   Fixes: 69007fce5002 ("messages: make profile pictures clickable to open in columns")

You can create this line with `git --no-pager show -s --pretty=fixes <commit>`
