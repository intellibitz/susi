#!/usr/bin/env bash
# Mandates 49-51: every commit belongs to a task from the shared queue, and a
# feature-branch push must already contain origin/main (sync-before-push is
# enforced by .githooks/workflow-guard before this script runs).
#
#   check-workflow-compliance.sh <base> <head> [remote] [branch]
#
# Each non-merge commit in <base>..<head> must carry a trailer
#     Task: T-<AGENT>-<n>
# naming a task that was OPEN in that commit's own tree and is held by a live
# claim (refs/claims/<id> on <remote>, lease not expired). A closed task is
# judged the same way: `susi tasks close` keeps the lease until the closing
# commit reaches main, so work -> close -> push passes, while a task file
# hand-moved into done/ (never claimed) does not. A commit citing a task
# already closed at that commit is refused. Exempt: merge commits,
# `chore: release vX.Y.Z`, github-actions[bot], and commits that touch only
# .agents/tasks/ (creating/closing a task).
#
# A live claim has an OWNER, and the owner reserved one branch plus a set of
# paths. So this also refuses:
#   (a) a commit for a task held on a DIFFERENT branch than the one being
#       pushed — without it a second agent can push work for a task someone
#       else already owns, which is exactly the duplicate work the queue
#       exists to prevent; and
#   (b) a commit touching files outside the claim's declared scopes, which is
#       how two agents end up editing the same file.
# Both are checked here, server-side, because the same scope check in
# .githooks/pre-commit (scripts/check-task-scope.py) is local and skippable
# with `--no-verify`, and because it can only see claims this worktree has
# already fetched. This script is what CI runs against the pushed branch.
#
# `branch` defaults to $GITHUB_REF_NAME (set by Actions) and then to the
# checked-out branch, so the local pre-push hook and CI agree without argument
# plumbing; when it cannot be determined the ownership check is skipped rather
# than guessed.
#
# Runs locally from .githooks/pre-push and server-side in CI ("Workflow
# Compliance"), where --no-verify cannot skip it.
set -uo pipefail

base=${1:?usage: check-workflow-compliance.sh <base> <head> [remote] [branch]}
head=${2:?usage: check-workflow-compliance.sh <base> <head> [remote] [branch]}
remote=${3:-${SUSI_TASK_REMOTE:-origin}}
branch=${4:-${GITHUB_REF_NAME:-$(git symbolic-ref -q --short HEAD 2>/dev/null || true)}}
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

# Prints a task's claim blob, or nothing when it has no claim ref.
claim_blob() {
    local id=$1 sha
    fetch_claims
    sha=$(git rev-parse --verify --quiet "refs/claims/$id" 2>/dev/null) || return 0
    git cat-file -p "$sha" 2>/dev/null || true
}

# Prints "live" / "expired" / "none" for a task's claim.
claim_state() {
    local body lease
    body=$(claim_blob "$1")
    [ -n "$body" ] || {
        echo none
        return
    }
    lease=$(jq -r '.lease_until_unix // 0' <<<"$body" 2>/dev/null || echo 0)
    if [ "${lease:-0}" -gt "$now" ]; then echo live; else echo expired; fi
}

# A live claim owns one branch and a set of paths: check both for this commit.
check_claim_owner() {
    local commit=$1 id=$2 short=$3 body owner_branch owner_agent scopes
    body=$(claim_blob "$id")
    owner_branch=$(jq -r '.branch // ""' <<<"$body" 2>/dev/null || true)
    owner_agent=$(jq -r '.agent // ""' <<<"$body" 2>/dev/null || true)
    if [ -n "$owner_branch" ] && [ -n "$branch" ] && [ "$owner_branch" != "$branch" ]; then
        err "commit $short works on $id, which $owner_agent holds on branch '$owner_branch', not '$branch' — one task belongs to one branch (susi tasks release $id, then claim it from your own branch)"
        return
    fi
    scopes=$(jq -r '.scopes[]?' <<<"$body" 2>/dev/null || true)
    [ -n "$scopes" ] || return 0
    local outside="" f s ok
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        case "$f" in .agents/tasks/*) continue ;; esac
        ok=0
        while IFS= read -r s; do
            [ -n "$s" ] || continue
            case "$f" in
            "$s" | "$s"/*)
                ok=1
                break
                ;;
            esac
        done <<<"$scopes"
        [ "$ok" = 1 ] || outside="$outside $f"
    done < <(git diff-tree --no-commit-id --name-only -r "$commit")
    [ -z "$outside" ] || err "commit $short touches files outside $id's claimed scopes:$outside — re-claim with a scope that covers them (susi tasks release $id, then susi tasks claim $id --scope <path>), or split the work into a task per area"
}

# The rule binds commits made after it was introduced: history that predates
# this script cannot be given trailers retroactively. It fails CLOSED: a range
# that cannot be resolved, or a branch that removed the script, is an error
# rather than a silent "nothing to check" — an unresolvable base used to make
# the loop below iterate zero commits and print its ✅ line.
head_commit=$(git rev-parse --verify --quiet "${head}^{commit}") || {
    echo "❌ workflow compliance: cannot resolve head '${head}'" >&2
    exit 1
}
base_commit=$(git rev-parse --verify --quiet "${base}^{commit}") || {
    echo "❌ workflow compliance: cannot resolve base '${base}' — refusing to certify an unknown range" >&2
    exit 1
}
cutover=$(git log --diff-filter=A --format=%H -1 "$head_commit" -- scripts/check-workflow-compliance.sh)
if [ -z "$cutover" ]; then
    # The script is not in this branch's history at all. That is either history
    # that predates the rule (exempt, and the reason the cutover exists) or a
    # branch that dropped the gate. The rule's own introduction on the base
    # tells them apart: if that commit is already an ancestor of head, this
    # branch had the script and removed it.
    introduced=$(git log --diff-filter=A --format=%H -1 "$base_commit" -- scripts/check-workflow-compliance.sh)
    if [ -n "$introduced" ] && git merge-base --is-ancestor "$introduced" "$head_commit" 2>/dev/null; then
        echo "❌ workflow compliance: this branch removed scripts/check-workflow-compliance.sh — the gate cannot be deleted by the branch it judges" >&2
        exit 1
    fi
    echo "✅ workflow compliance: $head predates the rule; nothing to check"
    exit 0
fi

commits=$(git rev-list --no-merges --reverse --ancestry-path "$cutover^..$head_commit" "^$base_commit") || {
    echo "❌ workflow compliance: cannot compute the commit range $base..$head" >&2
    exit 1
}
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
        # A closed task still has to be CLAIMED. `susi tasks close` keeps the
        # lease until the closing commit reaches main (tasks.rs:818), so the
        # normal work -> close -> push path satisfies this. Skipping the claim
        # check for a branch that merely ends with done/<id>.json accepted two
        # kinds of work that never went through the queue: a file hand-moved
        # into done/ without ever claiming the task, and a done-file that
        # arrived by merging main.
        case "$(claim_state "$task")" in
        live) check_claim_owner "$c" "$task" "$short" ;;
        expired) err "commit $short works on $task but its claim lease has expired — re-claim it (susi tasks claim $task)" ;;
        *) err "commit $short works on $task but nobody holds a claim on it — susi tasks claim $task first (Mandate 50)" ;;
        esac
    else
        err "commit $short names $task, which is not a task in this branch (add or merge the task file first)"
    fi
done <<<"$commits"

if [ "$fail" = 0 ]; then
    echo "✅ workflow compliance: every commit in $base..$head belongs to a claimed or closed task"
fi
exit "$fail"
