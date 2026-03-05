# Notedeck - Claude Code Guidelines

## Linear Integration

All work MUST be tracked through Linear:

- **Before starting work**: Search for an existing Linear issue first. If none exists, create one on the `notedeck` team.
- **Issue hierarchy**: For multi-step tasks, create a parent issue and sub-issues using `parentId`.
- **Status updates**: Move issues to "In Progress" when starting work, and "In Review" when complete. Humans do the final sign-off to "Done".
- **Commits**: Include the Linear issue git trailer at the end of the commit message, e.g., `Closes: https://linear.app/damus/issue/DECK-869` so that the issue can
be identified and closed automatically
- **Assignee**: Assign issues to the person doing the work. Use "me" for self-assignment.
- **Active project**: Current quarterly project is "1Q26 Notedeck". Assign issues to relevant milestones when applicable.
- **Teams**: `notedeck` (DECK) for app issues, `nostrdb` (NDB) for database issues.

## Committing

Before committing, you must format and fix clippy issues:

    cargo fmt --all && cargo clippy

