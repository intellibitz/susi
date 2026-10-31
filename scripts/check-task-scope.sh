#!/usr/bin/env bash
# Check staged files against a task's declared remote claim scopes.
set -euo pipefail
jq() { command jq "$@" | tr -d '\r'; }

task=${1:?task id required}
claim=$(git cat-file -p "refs/claims/$task" 2>/dev/null) || exit 0
jq -e 'type == "object"' <<<"$claim" >/dev/null 2>&1 || exit 0

owner_branch=$(jq -r '.branch // empty' <<<"$claim")
branch=$(git symbolic-ref -q --short HEAD || true)
if [ -n "$owner_branch" ] && [ "$owner_branch" != "$branch" ]; then
    echo "Task claim belongs to branch $owner_branch" >&2
    exit 1
fi

mapfile -t scopes < <(jq -r '.scopes // [] | .[] | select(type == "string" and length > 0)' <<<"$claim")

code=()
while IFS= read -r -d '' p; do
    [ -n "$p" ] || continue
    case "$p" in
    .agents/tasks/* | .agents/roadmap-verdicts/* | .agents/roadmap.json) continue ;;
    esac
    code+=("$p")
done < <(git diff --cached --name-only --no-renames -z)

if [ "${#scopes[@]}" -eq 0 ]; then
    if [ "${#code[@]}" -gt 0 ]; then
        joined=$(printf ', %s' "${code[@]}")
        joined=${joined#, }
        echo "Task $task reserves no paths — re-claim it with a scope that covers this change (susi tasks release $task, then susi tasks claim $task --scope <path>): $joined" >&2
        exit 1
    fi
    exit 0
fi

outside=()
for p in "${code[@]}"; do
    matched=0
    for s in "${scopes[@]}"; do
        if [ "$p" = "$s" ] || [[ "$p" == "$s/"* ]]; then
            matched=1
            break
        fi
    done
    if [ "$matched" -eq 0 ]; then
        outside+=("$p")
    fi
done

if [ "${#outside[@]}" -gt 0 ]; then
    joined=$(printf ', %s' "${outside[@]}")
    joined=${joined#, }
    echo "Files outside the task's reserved scopes: $joined" >&2
    exit 1
fi
exit 0
