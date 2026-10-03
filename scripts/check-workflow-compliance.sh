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
#       how two agents end up editing the same file; and
#   (c) any code at all under a claim that declared NO scopes. That used to
#       skip the test entirely, so the reservation was optional in practice —
#       claim without `--scope` and every path was fair game. Task records
#       (.agents/tasks/) and verdict records (.agents/roadmap-verdicts/) stay
#       exempt, so an unscoped legacy claim can still be created and closed, and
#       a verification verdict commits under the claim that produced it. A
#       verdict-only commit is still work: it keeps its `Task:` trailer.
#
# It also refuses work on a SECOND task by an agent that still owes the first
# one's merge. `susi tasks close` publishes a receipt (`refs/closed/<id>`) for
# the accepted-but-unmerged state, and the client refuses a new claim while one
# is outstanding — but that client is the agent's own binary, so a stale
# installed release, `--no-verify`, or a hand-pushed `refs/claims/<id>` all skip
# it. The rule can only fire where the client already refuses: a receipt whose
# close is on `origin/main` is published work, not debt, so the normal
# close -> merge -> next-task path never trips it.
#
# For the same reason it refuses an agent holding two live claims at once:
# `susi tasks claim` never creates that, but a hand-pushed claim ref can, and
# two live claims are two branches sharing one worker. Expired claims do not
# count — letting a lease lapse and claiming something else is the takeover
# path, not a second task.
#
# And it refuses a task DECLARED with an acceptance nobody else can re-run: a
# `scripts/` checker that is not on `origin/main` yet (the task would be writing
# the thing that declares it done), or a command that is not test-shaped. Only
# task files added in the pushed range are judged, so the existing queue stays
# closable.
#
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

closed_fetched=0

# Close receipts (`susi tasks close` pushes `refs/closed/<id>`) and a fresh
# `origin/main`: the two facts the owed-merge rule reasons over. Fetched once
# per run, and only when a live claim is actually examined.
fetch_closed() {
    [ "$closed_fetched" = 1 ] && return
    closed_fetched=1
    git fetch --quiet --prune "$remote" '+refs/closed/*:refs/closed/*' 2>/dev/null || true
    git fetch --quiet "$remote" 2>/dev/null || true
}

# The tasks this agent accepted and never published: a close receipt naming it,
# for a task other than the one in hand, whose close is not on `origin/main`.
# Prints nothing when the agent owes nothing.
owed_merges_of() {
    local agent=$1 exclude=$2 name id body owner
    while IFS= read -r name; do
        [ -n "$name" ] || continue
        id=${name#refs/closed/}
        [ "$id" != "$exclude" ] || continue
        body=$(git cat-file -p "$name" 2>/dev/null || true)
        owner=$(jq -r '.agent // ""' <<<"$body" 2>/dev/null || true)
        [ "$owner" = "$agent" ] || continue
        # A receipt whose close reached main is published work whose record has
        # simply not been cleaned up yet — never a reason to refuse a push.
        git cat-file -e "origin/main:.agents/tasks/done/$id.json" 2>/dev/null && continue
        printf '%s\n' "$id"
    done < <(git for-each-ref --format='%(refname)' refs/closed/ 2>/dev/null || true)
}

owed_reported=" "
# The server-side half of the claim gate in `susi tasks claim`: while an agent
# owes an earlier merge it may not start another task.
check_no_owed_merge() {
    local task=$1 short=$2 agent=$3 owed id
    [ -n "$agent" ] || return 0
    fetch_closed
    if ! git rev-parse --verify --quiet origin/main >/dev/null 2>&1; then
        echo "⚠️  workflow: origin/main is unresolvable; the owed-merge check is skipped" >&2
        return 0
    fi
    owed=$(owed_merges_of "$agent" "$task")
    for id in $owed; do
        [ -n "$id" ] || continue
        case "$owed_reported" in *" $agent:$id "*) continue ;; esac
        owed_reported="$owed_reported$agent:$id "
        err "commit $short works on $task while $agent still owes the merge of $id — an accepted task is not done until its close is on origin/main. Publish it (susi workflow finish $id) or give it up deliberately (susi tasks release $id --abandon <reason>) before starting another"
    done
}

agents_checked=" "
# One task at a time, per agent. `susi tasks claim` refuses to create a second
# live claim for one agent (an atomic `refs/claim-agents/<AGENT>` lease plus the
# live-claim scan), but a hand-pushed `refs/claims/<id>` can, and then two
# branches believe they own the same worker — the duplicate work the queue
# exists to prevent, and the reason one claim may not be renewed while another
# is live. Only NON-EXPIRED claims count: an agent that let a lease lapse and
# claimed something else is a legitimate state the takeover path exists for.
check_one_live_claim() {
    local agent=$1 short=$2 task=$3 name id body owner lease held n
    [ -n "$agent" ] || return 0
    case "$agents_checked" in *" $agent "*) return 0 ;; esac
    agents_checked="$agents_checked$agent "
    held=""
    while IFS= read -r name; do
        [ -n "$name" ] || continue
        id=${name#refs/claims/}
        body=$(git cat-file -p "$name" 2>/dev/null || true)
        owner=$(jq -r '.agent // ""' <<<"$body" 2>/dev/null || true)
        [ "$owner" = "$agent" ] || continue
        lease=$(jq -r '.lease_until_unix // 0' <<<"$body" 2>/dev/null || echo 0)
        [ "${lease:-0}" -gt "$now" ] || continue
        held="$held $id"
    done < <(git for-each-ref --format='%(refname)' refs/claims/ 2>/dev/null || true)
    n=0
    for _ in $held; do n=$((n + 1)); done
    [ "$n" -gt 1 ] && err "commit $short works on $task, but $agent holds $n live claims:$held — one task at a time: finish each with susi workflow finish <id> (or give it up with susi tasks release <id> --abandon <reason>) before claiming another"
    return 0
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
        err "commit $short works on $id, which $owner_agent holds on branch '$owner_branch', not '$branch' — one task belongs to one branch. Move it deliberately (release refuses a claim taken on another branch): susi tasks release $id --force, then susi tasks claim $id --scope <path> from your own branch"
        return
    fi
    scopes=$(jq -r '.scopes[]?' <<<"$body" 2>/dev/null || true)
    if [ -z "$scopes" ]; then
        # An empty scope list used to return here, which made the path
        # reservation optional in practice: claim without --scope and every
        # path was fair game, with neither this gate nor the pre-commit hook
        # able to tell two agents off one file. Refuse the code instead; task
        # records stay exempt, so an unscoped claim can still be closed.
        local unreserved="" uf
        while IFS= read -r uf; do
            [ -n "$uf" ] || continue
            case "$uf" in .agents/tasks/* | .agents/roadmap-verdicts/* | .agents/roadmap.json) continue ;; esac
            unreserved="$unreserved $uf"
        done < <(git diff-tree --no-commit-id --name-only -r "$commit")
        [ -z "$unreserved" ] || err "commit $short works on $id, whose claim reserves no paths:$unreserved — re-claim it with a scope that covers this change (susi tasks release $id --force, then susi tasks claim $id --scope <path>)"
        return
    fi
    local outside="" f s ok
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        case "$f" in .agents/tasks/* | .agents/roadmap-verdicts/* | .agents/roadmap.json) continue ;; esac
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
    [ -z "$outside" ] || err "commit $short touches files outside $id's claimed scopes:$outside — re-claim with a scope that covers them (susi tasks release $id --force, then susi tasks claim $id --scope <path>), or split the work into a task per area"
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
        live)
            check_claim_owner "$c" "$task" "$short"
            agent=$(jq -r '.agent // ""' <<<"$(claim_blob "$task")" 2>/dev/null || true)
            check_no_owed_merge "$task" "$short" "$agent"
            check_one_live_claim "$agent" "$short" "$task"
            ;;
        expired) err "commit $short works on $task but its claim lease has expired — re-claim it (susi tasks claim $task --scope <the paths this commit changes>)" ;;
        *) err "commit $short works on $task but nobody holds a claim on it — susi tasks claim $task --scope <path> first (Mandate 50)" ;;
        esac
    else
        err "commit $short names $task, which is not a task in this branch (add or merge the task file first)"
    fi
done <<<"$commits"

# A task's acceptance IS its definition of done, so the acceptance a task
# DECLARES has to be one somebody else can re-run. Otherwise a task can be
# declared done by a script written in the same branch, or by a command that
# verifies the host rather than the change — and no later gate can see it,
# because every later gate re-runs the same command. Judged on the task files
# ADDED in this range only: the queue that already exists, written by clients
# that predate this rule, stays closable.
while read -r c; do
    [ -n "$c" ] || continue
    short=${c:0:8}
    while IFS= read -r file; do
        [ -n "$file" ] || continue
        id=${file#.agents/tasks/}
        id=${id%.json}
        cmd=$(git show "$c:$file" 2>/dev/null | jq -r '.accept.cmd // [] | join(" ")' 2>/dev/null || true)
        if [ -z "$cmd" ]; then
            err "commit $short declares $id with no acceptance command — a task is not done without one (susi tasks add --accept \"<cmd>\")"
            continue
        fi
        case "$cmd" in
        "cargo test"* | "cargo nextest run"*) continue ;;
        esac
        program=${cmd%% *}
        case "$program" in
        scripts/*)
            # Unknown freshness is not a reason to refuse: an unreachable
            # origin/main only means this check cannot tell.
            git rev-parse --verify --quiet origin/main >/dev/null 2>&1 || continue
            git cat-file -e "origin/main:$program" 2>/dev/null && continue
            err "commit $short declares $id with acceptance \`$program\`, which is not a checker on origin/main — a task may not write the script that declares it done. Use a test (\`cargo test <filter>\`) or a checker already merged"
            ;;
        *)
            err "commit $short declares $id with acceptance \`$cmd\` — a new task must declare a test-shaped acceptance (\`cargo test <filter>\` / \`cargo nextest run -E test(<name>)\`) or a scripts/ checker already on origin/main, so the merged tree can re-run it"
            ;;
        esac
    done < <(git diff-tree --no-commit-id --diff-filter=A --name-only -r "$c" | grep -E '^\.agents/tasks/[^/]+\.json$' || true)
done <<<"$commits"

# Docs currency (advisory, never fatal): a task that changes the CLI surface but
# not the README leaves the front door describing a tool that no longer exists.
# It only warns — the README is prose, and a gate that forces prose produces
# worse prose than a task that lags.
range_files=$(git diff --name-only "$base_commit" "$head_commit" 2>/dev/null || true)
cli_files=$(printf '%s\n' "$range_files" | grep -c '^src/cli/' || true)
readme_files=$(printf '%s\n' "$range_files" | grep -c '^README\.md$' || true)
if [ "$cli_files" -gt 0 ] && [ "$readme_files" -eq 0 ]; then
    echo "⚠️  docs: $cli_files file(s) under src/cli/ changed without README.md — if a command, flag or" >&2
    echo "   output changed, document it in the same task (advisory: docs may lag a task, not a release)" >&2
fi

if [ "$fail" = 0 ]; then
    echo "✅ workflow compliance: every commit in $base..$head belongs to a claimed or closed task"
fi
exit "$fail"
