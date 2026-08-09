#!/usr/bin/env bash
#
# Share one Cargo target/ directory across every git worktree.
#
# The main worktree keeps the real target/; every other worktree's target/
# becomes a relative symlink pointing at it, so all worktrees share a single
# build cache instead of each carrying its own multi-gigabyte copy.
#
# Usage:
#   scripts/worktree-targets.sh [status]   # show each worktree's target/ state (default)
#   scripts/worktree-targets.sh relink     # (re)create the symlinks, replacing stray real dirs
#   scripts/worktree-targets.sh nuke       # cargo clean the shared target, then relink
#
# Safe to run from any worktree. `relink` and `status` are idempotent and
# non-destructive to build outputs; only `nuke` clears the shared cache.

set -euo pipefail

# --- locate the main worktree (it owns the shared target/) -------------------
# --git-common-dir resolves to <main-worktree>/.git from anywhere in any worktree.
common_git=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || {
    echo "error: not inside a git repository" >&2
    exit 1
}
main_wt=$(dirname "$common_git")
main_target="$main_wt/target"

# List every worktree path (main is always first).
worktree_paths() {
    git worktree list --porcelain | awk '/^worktree /{print $2}'
}

# Remove a path even if a background tool (rust-analyzer, cargo) races to
# recreate it mid-delete — retry once before giving up.
force_remove() {
    local p=$1
    rm -rf "$p" 2>/dev/null || true
    if [ -e "$p" ] || [ -L "$p" ]; then
        rm -rf "$p"
    fi
}

# Ensure "$wt/target" is a relative symlink to the shared target.
# Skips the main worktree. Returns via stdout what it did.
link_one() {
    local wt=$1
    local target="$wt/target"

    if [ "$wt" = "$main_wt" ]; then
        printf '  %-32s real target (canonical)\n' "$(basename "$wt")"
        return
    fi

    local rel
    rel=$(realpath --relative-to="$wt" "$main_target")

    # Already the correct symlink? Nothing to do.
    if [ -L "$target" ] && [ "$(readlink "$target")" = "$rel" ]; then
        printf '  %-32s already linked -> %s\n' "$(basename "$wt")" "$rel"
        return
    fi

    # Replace whatever is there (stray real dir, wrong/dangling symlink).
    if [ -d "$target" ] && [ ! -L "$target" ]; then
        printf '  %-32s replacing real dir (%s) with symlink\n' \
            "$(basename "$wt")" "$(du -sh "$target" 2>/dev/null | cut -f1)"
    fi
    force_remove "$target"
    ln -s "$rel" "$target"
    printf '  %-32s linked -> %s\n' "$(basename "$wt")" "$rel"
}

cmd_status() {
    echo "shared target: $main_target"
    local p
    while IFS= read -r p; do
        local target="$p/target"
        if [ "$p" = "$main_wt" ]; then
            printf '  %-32s real target (%s) [canonical]\n' \
                "$(basename "$p")" "$(du -sh "$target" 2>/dev/null | cut -f1 || echo missing)"
        elif [ -L "$target" ]; then
            if [ -d "$target/." ]; then
                printf '  %-32s symlink -> %s\n' "$(basename "$p")" "$(readlink "$target")"
            else
                printf '  %-32s DANGLING -> %s\n' "$(basename "$p")" "$(readlink "$target")"
            fi
        elif [ -d "$target" ]; then
            printf '  %-32s REAL DIR (%s) [not linked]\n' \
                "$(basename "$p")" "$(du -sh "$target" 2>/dev/null | cut -f1)"
        else
            printf '  %-32s (no target)\n' "$(basename "$p")"
        fi
    done < <(worktree_paths)
}

cmd_relink() {
    echo "relinking worktree targets -> $main_target"
    # Keep the shared target present so fresh links resolve immediately.
    mkdir -p "$main_target"
    local p
    while IFS= read -r p; do
        link_one "$p"
    done < <(worktree_paths)
}

cmd_nuke() {
    echo "nuking shared target: cargo clean in $main_wt"
    ( cd "$main_wt" && cargo clean )
    cmd_relink
}

case "${1:-status}" in
    status) cmd_status ;;
    relink) cmd_relink ;;
    nuke)   cmd_nuke ;;
    -h|--help|help)
        sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
        ;;
    *)
        echo "error: unknown command '$1' (use: status | relink | nuke)" >&2
        exit 1
        ;;
esac
