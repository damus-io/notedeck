# Notedeck - Claude Code Guidelines

## Work Tracking

All work is tracked on the **Headway board** via the `headway` CLI, not GitHub
Issues. See the `headway` skill (`.claude/skills/headway/SKILL.md`) for the full
command reference; the board flows `Backlog → Todo → In Progress → Done`.

- **Before starting work**: `headway show` to read the board. If a card for the
  work exists, move it to In Progress: `headway move <card> --col in-progress`.
  If none exists, add one: `headway add "<title>" --col in-progress`.
- **Task breakdown**: Use one card per unit of work; `desc`/`label` for detail.
- **On completion**: Move the card to Done: `headway move <card> --col done`.
- Cards are addressed by a short id prefix (from `show`); always `show` before
  editing. The CLI reads the signing key from `$HEADWAY_NSEC`.

## Committing

Before committing, run the local CI checks:

    ./scripts/ci-local

This runs changelog trailer checks, lint (fmt + clippy), tests, and the android build — all parsed directly from the GitHub workflow YAML. You can also run individual jobs via `./scripts/ci.py`, e.g. `./scripts/ci.py lint`.

After a change lands, move its Headway card to Done (`headway move <card> --col done`).

Every commit must include a Changelog git trailer. For user-facing changes, use `Changelog-{Added,Changed,Fixed,Removed}`. For internal changes (refactors, CI, tooling, docs, etc), use `Changelog-None:`. Examples:

   Changelog-Added: Add new zap metadata stats on notes

Bug fixes

   Changelog-Fixed: Fix a bug with foo's not toggling the bars

Or if there's nothing interesting to note (refactors, etc)

   Changelog-None:

When fixing a bug introduced by another commit, add:

   Fixes: 69007fce5002 ("messages: make profile pictures clickable to open in columns")

You can create this line with `git --no-pager show -s --pretty=fixes <commit>`
