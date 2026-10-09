#!/usr/bin/env bash
#
# What agents wanted and worked around, most-wanted first. AGENTS.md says how a
# want is recorded; a want's count is the cards that wanted it.
#
# Usage:
#   scripts/hindsight.sh          # every want: its count, its file and its title
#   scripts/hindsight.sh top [N]  # the top N wants' files whole (default 3), with
#                                 # counts, then the rest one line each, as markdown
#   scripts/hindsight.sh <want>   # one want's file: which cards wanted it, and why
set -euo pipefail
cd "$(dirname "$0")/../docs/hindsight"
shopt -s nullglob

ranked() {
    for f in *.md; do
        printf '%3d  %-38s %s\n' "$(grep -c '^- ' "$f")" "${f%.md}" "$(head -1 "$f" | sed 's/^# //')"
    done | sort -k1,1nr -k2,2
}

if [ "${1:-}" = top ]; then
    n=${2:-3}
    i=0
    while read -r count want _; do
        i=$((i + 1))
        if [ "$i" -le "$n" ]; then
            # The title, the count under it, then the file from its blank line on.
            head -1 "$want.md"
            echo "**$count cards**"
            tail -n +2 "$want.md"
            echo
        else
            [ "$i" -eq $((n + 1)) ] && printf '## The rest\n\n'
            # The header's summary lines, without its Card: line, joined into one.
            said=$(awk 'NR > 2 && /^$/ { exit } NR > 2 && !/^Card:/' "$want.md" | paste -sd' ')
            echo "- **$count** — $said"
        fi
    done < <(ranked)
    exit
fi

if [ $# -gt 0 ]; then
    cat "${1%.md}.md"
    exit
fi

ranked
