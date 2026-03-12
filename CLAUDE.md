# Notedeck - Claude Code Guidelines

## GitHub Issue Tracking

All work MUST be tracked through GitHub Issues on `damus-io/notedeck`:

- **Before starting work**: Search for an existing GitHub issue first using `gh issue list --search "keywords"`. If none exists, create one with `gh issue create`.
- **Task breakdown**: For multi-step tasks, use GitHub task lists (checkboxes) in the issue body or create linked sub-issues.
- **Labels**: Use labels to categorize issues (bug, enhancement, etc.).
- **Assignee**: Assign issues to the person doing the work.
- **Close on merge**: Reference issues in commit messages or PR descriptions (e.g., `Fixes #123`, `Closes #456`) so they close automatically when merged.

## Committing

Before committing, run the local CI checks:

    ./scripts/ci-local

This runs changelog trailer checks, lint (fmt + clippy), tests, and the android build — all parsed directly from the GitHub workflow YAML. You can also run individual jobs via `./scripts/ci.py`, e.g. `./scripts/ci.py lint`.

Reference the GitHub issue number in the commit message or PR description (e.g., `Fixes #123`) so issues close automatically on merge.

Every commit must include a Changelog git trailer. For user-facing changes, use `Changelog-{Added,Changed,Fixed,Removed}`. For internal changes (refactors, CI, tooling, docs, etc), use `Changelog-None:`. Examples:

   Changelog-Added: Add new zap metadata stats on notes

Bug fixes

   Changelog-Fixed: Fix a bug with foo's not toggling the bars

Or if there's nothing interesting to note (refactors, etc)

   Changelog-None:


