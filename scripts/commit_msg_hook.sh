#!/usr/bin/env bash

# Validates that commit messages contain a required Changelog trailer.
# Used as a commit-msg hook and in CI.

set -euo pipefail

check_changelog_trailer() {
    local msg="$1"

    # Skip merge commits
    if echo "$msg" | head -1 | grep -qE '^Merge '; then
        return 0
    fi

    if echo "$msg" | grep -qE '^Changelog-(Added|Changed|Fixed|Removed|None):'; then
        return 0
    fi

    echo "WARNING: Commit message is missing a Changelog trailer."
    echo "Your PR will fail CI without one."
    echo ""
    echo "Add one of the following to the end of your commit message:"
    echo "  Changelog-Added: <description>"
    echo "  Changelog-Changed: <description>"
    echo "  Changelog-Fixed: <description>"
    echo "  Changelog-Removed: <description>"
    echo "  Changelog-None:"
    echo ""
    return 1
}

# When called as a git hook, the commit message file is passed as $1
if [ "${1:-}" != "" ] && [ -f "${1:-}" ]; then
    msg=$(cat "$1")
    # Warn but don't block the commit — CI enforces this strictly
    check_changelog_trailer "$msg" || true
else
    # When used in CI, read from stdin
    while IFS= read -r msg; do
        if ! check_changelog_trailer "$msg"; then
            exit 1
        fi
    done
fi
