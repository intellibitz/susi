#!/usr/bin/env bash
# Mandates 49-51: every commit belongs to a task from the shared queue, and a
# feature-branch push must already contain origin/main (sync-before-push is
# enforced by .githooks/workflow-guard before this script runs).
#
#   check-workflow-compliance.sh <base> <head> [remote]
#
# Each non-merge commit in <base>..<head> must carry a trailer
#     Task: T-<AGENT>-<n>
# naming a task that was OPEN in that commit's own tree and either is held by a
# live claim (refs/claims/<id> on <remote>, lease not expired) or was closed by
# the same branch (.agents/tasks/done/<id>.json at <head>). A commit citing a
# task already closed at that commit is refused. Exempt: merge commits, `chore: release vX.Y.Z`, github-actions[bot],
# and commits that touch only .agents/tasks/ (creating/closing a task).
#
# Runs locally from .githooks/pre-push and server-side in CI ("Workflow
# Compliance"), where --no-verify cannot skip it.
set -uo pipefail

base=${1:?usage: check-workflow-compliance.sh <base> <head> [remote]}
head=${2:?usage: check-workflow-compliance.sh <base> <head> [remote]}
remote=${3:-${SUSI_TASK_REMOTE:-origin}}
now=$(date +%s)
fail=0
claims_fetched=0

err() { echo "❌ workflow: $*" >&2; fail=1; }

fetch_claims() {
    [ "$claims_fetched" = 1 ] && return
    claims_fetched=1
    git ls-remote "$remote" 'refs/claims/*' 2>/dev/null | grep -q . || return 0
    git fetch --quiet --prune "$remote" '+refs/claims/*:refs/claims/*' 2>/dev/null || true
}

# Prints "live" / "expired" / "none" for a task's claim.
claim_state() {
    local id=$1 sha lease
    fetch_claims
    sha=$(git rev-parse --verify --quiet "refs/claims/$id" 2>/dev/null) || { echo none; return; }
    lease=$(git cat-file -p "$sha" 2>/dev/null | jq -r '.lease_until_unix // 0' 2>/dev/null || echo 0)
    if [ "${lease:-0}" -gt "$now" ]; then echo live; else echo expired; fi
}

# The rule binds commits made after it was introduced: history that predates
# this script cannot be given trailers retroactively.
cutover=$(git log --diff-filter=A --format=%H -1 "$head" -- scripts/check-workflow-compliance.sh)
if [ -z "$cutover" ]; then
    echo "✅ workflow compliance: $head predates the rule; nothing to check"
    exit 0
fi

while read -r c; do
    [ -n "$c" ] || continue
    short=${c:0:8}
    subject=$(git log -1 --format=%s "$c")
    author=$(git log -1 --format=%an "$c")
    case "$subject" in "chore: release v"*) continue ;; esac
    [ "$author" = "github-actions[bot]" ] && continue
    # Creating or closing a task touches only the task files.
    non_task=$(git diff-tree --no-commit-id --name-only -r "$c" | grep -vc '^\.agents/tasks/' || true)
    total=$(git diff-tree --no-commit-id --name-only -r "$c" | grep -c . || true)
    if [ "$total" -gt 0 ] && [ "$non_task" = 0 ]; then continue; fi

    task=$(git log -1 --format=%B "$c" | sed -n 's/^Task: \(T-[A-Z0-9][A-Z0-9]*-[0-9][0-9]*\)[[:space:]]*$/\1/p' | head -1)
    if [ -z "$task" ]; then
        err "commit $short \"$subject\" has no 'Task: T-<AGENT>-<n>' trailer (Mandate 50: work comes from the queue — susi tasks add/claim)"
        continue
    fi
    # Judge the commit against its own tree: it must be work on a task that
    # was still open there. Citing an already-closed task would let unrelated
    # work ride on it.
    if git cat-file -e "$c:.agents/tasks/done/$task.json" 2>/dev/null; then
        err "commit $short cites $task, which was already closed at that commit — add a new task (susi tasks add) instead"
    elif git cat-file -e "$c:.agents/tasks/$task.json" 2>/dev/null; then
        if git cat-file -e "$head:.agents/tasks/done/$task.json" 2>/dev/null; then
            : # this branch went on to close it: the claim was released on close
        else
            case "$(claim_state "$task")" in
            live) ;;
            expired) err "commit $short works on $task but its claim lease has expired — re-claim it (susi tasks claim $task)" ;;
            *) err "commit $short works on $task but nobody holds a claim on it — susi tasks claim $task first (Mandate 50)" ;;
            esac
        fi
    else
        err "commit $short names $task, which is not a task in this branch (add or merge the task file first)"
    fi
done < <(git rev-list --no-merges --reverse --ancestry-path "$cutover^..$head" "^$base")

if [ "$fail" = 0 ]; then
    echo "✅ workflow compliance: every commit in $base..$head belongs to a claimed or closed task"
fi
exit "$fail"
