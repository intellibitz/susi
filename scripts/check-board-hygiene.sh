#!/usr/bin/env bash
# Board hygiene: merged remote branches and claim refs nobody can act on.
#
# Usage:
#   scripts/check-board-hygiene.sh            # report; exit 1 when dirty
#   scripts/check-board-hygiene.sh --apply    # remove what it reported
set -euo pipefail
jq() { command jq "$@" | tr -d '\r'; }

NAME="board hygiene"
TMP_NAMESPACE="refs/board-hygiene/claims"
remote="origin"
apply=0
for arg in "$@"; do
    [ "$arg" = "--apply" ] && apply=1
done

drop_tmp() {
    local ref
    while IFS= read -r ref; do
        [ -n "$ref" ] && git update-ref -d "$ref" 2>/dev/null || true
    done < <(git for-each-ref --format='%(refname)' "$TMP_NAMESPACE")
}
trap drop_tmp EXIT

now=$(date +%s)
git fetch --quiet --prune "$remote"
git fetch --quiet --prune "$remote" "+refs/claims/*:$TMP_NAMESPACE/*" 2>/dev/null || true

task_state() {
    local task=$1 base="refs/remotes/$remote/main"
    if git cat-file -e "$base:.agents/tasks/$task.json" 2>/dev/null; then
        echo "open"
    elif git cat-file -e "$base:.agents/tasks/done/$task.json" 2>/dev/null; then
        echo "closed"
    else
        echo "missing"
    fi
}

claims_count=0
dead_tasks=()
dead_agents=()
dead_states=()
lapsed_tasks=()
lapsed_agents=()
all_claim_tasks=()
all_claim_agents=()

while IFS= read -r ref; do
    [ -n "$ref" ] || continue
    claims_count=$((claims_count + 1))
    task=${ref#"$TMP_NAMESPACE/"}
    body=$(git cat-file -p "$ref" 2>/dev/null || echo "{}")
    lease=$(jq -r '.lease_until_unix // 0' <<<"$body" 2>/dev/null || echo 0)
    agent=$(jq -r '.agent // "?"' <<<"$body" 2>/dev/null || echo "?")
    all_claim_tasks+=("$task")
    all_claim_agents+=("$agent")
    if [[ "$lease" =~ ^[0-9]+$ ]] && [ "$lease" -gt "$now" ]; then
        continue
    fi
    state=$(task_state "$task")
    if [ "$state" = "open" ]; then
        lapsed_tasks+=("$task")
        lapsed_agents+=("$agent")
    else
        dead_tasks+=("$task")
        dead_agents+=("$agent")
        dead_states+=("$state")
    fi
done < <(git for-each-ref --format='%(refname)' "$TMP_NAMESPACE")

merged_names=()
merged_shas=()
prefix="refs/remotes/$remote/"
while IFS=' ' read -r name sha; do
    [ -n "$name" ] && [ -n "$sha" ] || continue
    short=${name#"$prefix"}
    if [ "$short" = "$name" ] || [ -z "$short" ] || [ "$short" = "main" ] || [ "$short" = "HEAD" ]; then
        continue
    fi
    if git merge-base --is-ancestor "$sha" "$remote/main" 2>/dev/null; then
        merged_names+=("$short")
        merged_shas+=("$sha")
    fi
done < <(git for-each-ref --format='%(refname) %(objectname)' "$prefix")

for (( i = 0; i < ${#lapsed_tasks[@]}; i++ )); do
    echo "⚠️  $NAME: ${lapsed_tasks[$i]} (${lapsed_agents[$i]}) has a lapsed claim on a task that is still open — claimable, not debris: susi tasks claim ${lapsed_tasks[$i]}"
done
for (( i = 0; i < ${#dead_tasks[@]}; i++ )); do
    echo "❌ $NAME: claim ref ${dead_tasks[$i]} (${dead_agents[$i]}, task ${dead_states[$i]}) is dead — git push $remote :refs/claims/${dead_tasks[$i]}"
done
for (( i = 0; i < ${#merged_names[@]}; i++ )); do
    sha8=${merged_shas[$i]:0:8}
    echo "❌ $NAME: ${merged_names[$i]} ($sha8) is merged into $remote/main — git push $remote --delete ${merged_names[$i]}"
done

if [ "${#dead_tasks[@]}" -eq 0 ] && [ "${#merged_names[@]}" -eq 0 ]; then
    echo "✅ $NAME: no merged branches, no dead claim refs ($claims_count claim ref(s) read)"
    exit 0
fi

if [ "$apply" -eq 0 ]; then
    printf '\n%d dead claim ref(s), %d merged branch(es); re-run with --apply to remove them\n' "${#dead_tasks[@]}" "${#merged_names[@]}"
    exit 1
fi

for task in "${dead_tasks[@]}"; do
    git push --quiet "$remote" ":refs/claims/$task"
done

for dead_agent in $(printf '%s\n' "${dead_agents[@]}" | sort -u); do
    [ -n "$dead_agent" ] && [ "$dead_agent" != "?" ] || continue
    survives=0
    for (( i = 0; i < ${#all_claim_tasks[@]}; i++ )); do
        t=${all_claim_tasks[$i]}
        a=${all_claim_agents[$i]}
        [ "$a" = "$dead_agent" ] || continue
        is_dead=0
        for dt in "${dead_tasks[@]}"; do
            if [ "$dt" = "$t" ]; then
                is_dead=1
                break
            fi
        done
        if [ "$is_dead" -eq 0 ]; then
            survives=1
            break
        fi
    done
    if [ "$survives" -eq 0 ]; then
        git push --quiet "$remote" ":refs/claim-agents/$dead_agent" 2>/dev/null || true
    fi
done

for short in "${merged_names[@]}"; do
    git push --quiet "$remote" --delete "$short"
done

echo "removed ${#dead_tasks[@]} dead claim ref(s) and ${#merged_names[@]} merged branch(es)"
exit 0
