# Notedeck - Claude Code Guidelines

## Linear Integration

All work MUST be tracked through Linear:

- **Before starting work**: Search for an existing Linear issue first. If none exists, create one on the `notedeck` team.
- **Issue hierarchy**: For multi-step tasks, create a parent issue and sub-issues using `parentId`.
- **Status updates**: Move issues to "In Progress" when starting work, and "In Review" when complete. Humans do the final sign-off to "Done".
- **Assignee**: Assign issues to the person doing the work. Use "me" for self-assignment.
- **Active project**: Current quarterly project is "1Q26 Notedeck". Assign issues to relevant milestones when applicable.
- **Teams**: `notedeck` (DECK) for app issues, `nostrdb` (NDB) for database issues.

## Committing

Before committing, you must format and fix clippy issues:

    cargo fmt --all && cargo clippy

Include the Linear issue identifier in the commit message or branch name (e.g., `DECK-869`) so that the GitHub-Linear sync can track it automatically.

Every commit must include a Changelog git trailer. For user-facing changes, use `Changelog-{Added,Changed,Fixed,Removed}`. For internal changes (refactors, CI, tooling, docs, etc), use `Changelog-None:`. Examples:

   Changelog-Added: Add new zap metadata stats on notes

Bug fixes

   Changelog-Fixed: Fix a bug with foo's not toggling the bars

Or if there's nothing interesting to note (refactors, etc)

   Changelog-None:


