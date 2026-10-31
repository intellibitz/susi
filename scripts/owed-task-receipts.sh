#!/usr/bin/env bash
# Print task IDs closed by <agent> whose close is not present on origin/main.
# Read all close receipt blobs in one Git process and parse them in one jq run.
set -uo pipefail
[ ! -f "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh" ] || . "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh"

agent=${1:?usage: owed-task-receipts.sh <agent>}
main_commit=$(git rev-parse --verify --quiet 'refs/remotes/origin/main^{commit}' 2>/dev/null || true)
refs=$(git for-each-ref --format='%(refname:strip=2) %(objectname)' refs/closed/ 2>/dev/null || true)
[ -n "$refs" ] || exit 0

ids=$(awk '{print $1}' <<<"$refs")
objects=$(awk '{print $2}' <<<"$refs")
# Receipts are single-line JSON blobs, with or without a final newline.
# Discard batch headers and separators rather than assuming a fixed line count.
bodies=$(printf '%s\n' "$objects" | git cat-file --batch 2>/dev/null | awk 'NF && !/^[0-9a-f]+ blob [0-9]+$/ { print }')
pairs=$(paste <(printf '%s\n' "$ids") <(printf '%s\n' "$bodies"))
jq -Rrn --arg agent "$agent" \
    '[inputs | split("\t") as $p | ($p[1] | fromjson? // {}) as $receipt | select($receipt.agent == $agent) | $p[0]] | .[]' \
    <<<"$pairs" 2>/dev/null | while IFS= read -r id; do
        [ -n "$id" ] || continue
        if [ -n "$main_commit" ] && git cat-file -e "$main_commit:.agents/tasks/done/$id.json" 2>/dev/null; then
            : # Published close; not outstanding merge debt.
        else
            printf '%s\n' "$id"
        fi
    done
