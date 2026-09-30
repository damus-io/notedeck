#!/usr/bin/env bash
#
# Keep every git worktree's Cargo target/ on /titan, one directory each.
#
# Each worktree's target/ becomes a symlink to $TARGET_ROOT/<worktree>/target
# (default /titan/dev/<main worktree's name>, so /titan/dev/notedeck/<worktree>/
# target). <worktree> is the directory's name for a sibling of the main
# worktree, and that name plus a short hash of its path anywhere else, so two
# scratch checkouts both called wt do not meet.
#
# Targets are not shared. Cargo names a workspace crate's artifacts by the
# crate's path relative to the workspace root, so every worktree writes the
# same file names into a shared target. A binary another worktree built is
# then newer than this worktree's sources and is taken as fresh, and a test
# that reads source through env!("CARGO_MANIFEST_DIR") checks the other
# worktree's tree instead of this one. In microverse that hid two failing
# checks; here it is why bin_testing::worktree_bin exists. It costs one cold
# build per worktree.
#
# Usage:
#   scripts/worktree-targets.sh [status]   # show each worktree's target/ (default)
#   scripts/worktree-targets.sh relink     # (re)create the symlinks, adopting old builds
#   scripts/worktree-targets.sh prune      # delete targets whose worktree is gone
#   scripts/worktree-targets.sh nuke       # delete this worktree's target, then relink
#
# Safe to run from any worktree, and idempotent. relink never deletes a build:
# it swaps links by rename, so a running build never sees target/ missing, and
# it names anything it leaves stale for you to delete.

set -euo pipefail

common_git=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || {
    echo "error: not inside a git repository" >&2
    exit 1
}
main_wt=$(dirname "$common_git")
root=${TARGET_ROOT:-/titan/dev/$(basename "$main_wt")}

worktree_paths() {
    git worktree list --porcelain | awk '/^worktree /{print $2}'
}

# Where worktree $1's target/ lives.
home_of() {
    local name
    name=$(basename "$1")
    if [ "$(dirname "$1")" != "$(dirname "$main_wt")" ]; then
        name="$name-$(printf '%s' "$1" | sha1sum | cut -c1-8)"
    fi
    echo "$root/$name/target"
}

# Point symlink $2 at $1 by renaming a fresh link over it, never unlinking
# first, so the path always resolves.
swap_link() {
    local tmp="$2.relink.$$"
    ln -sfn "$1" "$tmp"
    mv -T "$tmp" "$2"
}

# Move the build directory $1 (the old shared target) to $2 in one atomic
# exchange with a symlink to $2, so both the old path and every link through
# it keep resolving. Returns nonzero, touching nothing, if they are on
# different filesystems.
adopt() {
    ln -sfn "$2" "$2"   # placeholder the exchange puts at $1's path
    if mv -T --exchange "$1" "$2" 2>/dev/null; then
        return 0
    fi
    rm -f "$2"
    return 1
}

# Size of directory $1, or "missing".
size_of() {
    du -sh "$1/" 2>/dev/null | cut -f1 || echo missing
}

cmd_status() {
    echo "targets under: $root"
    local wt live=""
    while IFS= read -r wt; do
        local target="$wt/target" want
        want=$(home_of "$wt")
        live+="$(dirname "$want")"$'\n'
        if [ -L "$target" ] && [ "$(readlink "$target")" = "$want" ]; then
            if [ -d "$want" ]; then
                printf '  %-28s -> %s (%s)\n' "$(basename "$wt")" "$want" "$(size_of "$want")"
            else
                printf '  %-28s -> %s (MISSING, run relink)\n' "$(basename "$wt")" "$want"
            fi
        elif [ -L "$target" ]; then
            printf '  %-28s WRONG LINK -> %s\n' "$(basename "$wt")" "$(readlink "$target")"
        elif [ -d "$target" ]; then
            printf '  %-28s REAL DIR (%s)\n' "$(basename "$wt")" "$(size_of "$target")"
        else
            printf '  %-28s (no target)\n' "$(basename "$wt")"
        fi
    done < <(worktree_paths)

    # Anything else under the root: a pruneable target, or something relink
    # did not make (prune leaves those alone).
    local entry
    for entry in "$root"/*; do
        [ -e "$entry" ] || [ -L "$entry" ] || continue
        grep -qxF "$entry" <<<"$live" && continue
        if [ -L "$entry" ]; then
            printf '  stale: %s -> %s\n' "$entry" "$(readlink "$entry")"
        elif [ -f "$entry/.worktree" ]; then
            printf '  pruneable: %s (%s, worktree %s is gone)\n' \
                "$entry" "$(size_of "$entry")" "$(cat "$entry/.worktree")"
        else
            printf '  stale: %s (%s, not made by relink)\n' "$entry" "$(size_of "$entry")"
        fi
    done
}

cmd_relink() {
    local wt
    while IFS= read -r wt; do
        local target="$wt/target" want cur
        want=$(home_of "$wt")
        mkdir -p "$(dirname "$want")"
        # Whose it is, so prune touches only what relink made.
        printf '%s\n' "$wt" > "$(dirname "$want")/.worktree"
        if [ -L "$target" ] && [ "$(readlink "$target")" = "$want" ]; then
            mkdir -p "$want"
            printf '  %-28s already linked\n' "$(basename "$wt")"
            continue
        fi

        if [ -d "$target" ] && [ ! -L "$target" ]; then
            # This worktree's own build: keep it, unless one is already there.
            if [ -e "$want" ]; then
                printf '  %-28s REAL DIR kept, %s already exists; delete one and rerun\n' \
                    "$(basename "$wt")" "$want"
                continue
            fi
            printf '  %-28s moving real dir (%s)\n' "$(basename "$wt")" "$(size_of "$target")"
            mv -T "$target" "$want"
            ln -s "$want" "$target"
            printf '  %-28s linked -> %s\n' "$(basename "$wt")" "$want"
            continue
        fi

        # A link into a build relink did not make (the old shared target) is
        # adopted by the first worktree to reach it, the main one; the rest
        # then find it marked as the main worktree's and start fresh.
        cur=$(realpath -e "$target" 2>/dev/null || true)
        if [ ! -e "$want" ] && [ -n "$cur" ] && [ -d "$cur" ] \
            && [ ! -f "$(dirname "$cur")/.worktree" ]; then
            if adopt "$cur" "$want"; then
                printf '  %-28s adopted %s (%s)\n' "$(basename "$wt")" "$cur" "$(size_of "$want")"
                printf '  %-28s stale: %s -> %s, delete once no build uses it\n' \
                    "" "$cur" "$want"
            else
                printf '  %-28s left %s in place (other filesystem), stale: delete it\n' \
                    "$(basename "$wt")" "$cur"
            fi
        fi
        mkdir -p "$want"
        swap_link "$want" "$target"
        printf '  %-28s linked -> %s\n' "$(basename "$wt")" "$want"
    done < <(worktree_paths)
}

cmd_prune() {
    local live dir
    live=$(worktree_paths)
    for dir in "$root"/*/; do
        dir=${dir%/}
        [ -L "$dir" ] && continue
        [ -f "$dir/.worktree" ] || continue
        if ! grep -qxF "$(cat "$dir/.worktree")" <<<"$live"; then
            printf '  removing %s (%s)\n' "$dir" "$(size_of "$dir")"
            rm -rf "$dir"
        fi
    done
}

cmd_nuke() {
    local wt want
    wt=$(git rev-parse --show-toplevel)
    want=$(home_of "$wt")
    # Not cargo clean: through the symlink it removes only the link, and it
    # refuses the real directory, which relink made without a CACHEDIR.TAG.
    printf '  removing %s (%s)\n' "$want" "$(size_of "$want")"
    rm -rf "$want"
    cmd_relink
}

case "${1:-status}" in
    status) cmd_status ;;
    relink) cmd_relink ;;
    prune)  cmd_prune ;;
    nuke)   cmd_nuke ;;
    -h|--help|help) sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//' ;;
    *)
        echo "error: unknown command '$1' (use: status | relink | prune | nuke)" >&2
        exit 1
        ;;
esac
