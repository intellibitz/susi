#!/usr/bin/env bash
# Safe boundaries for the per-task agent loop. Never merge into dirty worktrees.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
common=$(git rev-parse --path-format=absolute --git-common-dir)
action=${1:-sync}
lock="$common/susi-workflow.lock"
susi_bin=${SUSI_WORKFLOW_BIN:-susi}

sync() {
    [ "$(git rev-parse --path-format=absolute --git-dir)" != "$common" ] || { echo 'Use your own linked worktree.' >&2; return 1; }
    [ "$(git symbolic-ref -q --short HEAD)" != main ] || { echo 'Use your own branch.' >&2; return 1; }
    exec 9>"$lock"
    flock 9
    [ -z "$(git status --porcelain)" ] || { echo 'Commit or stash your work before syncing.' >&2; return 1; }
    git fetch --quiet origin
    git merge --no-edit origin/main
    "$root/scripts/park-primary.sh" --sync-only
    # Inside the lock: the watcher heal is cheap, and the board block is cached,
    # so one sync pays for the checks and the rest read the verdict.
    #
    # The watcher is a background process that dies with its session, and the
    # primary then stops converging until someone notices. Every agent crosses
    # this boundary, so the loop heals it here. Non-fatal either way.
    "$root/scripts/ensure-watcher.sh" || true
    # A working agent crosses this boundary but not `finish`, so top the lease up.
    renew_owned_claims
    # The board checks were only as reliable as the habit of running them.
    "$root/scripts/swarm-status.sh" || true
    flock -u 9
}

# Keep the claim alive across a long wait. `renew` refuses an expired lease, and
# under `set -e` that used to abort finish mid-wait — the PR stranded, the task
# no longer owned by anyone, and its work free for another agent to redo. A
# lapsed lease is recoverable: take the same task back, keeping its scopes.
renew_or_readopt() {
    "$susi_bin" tasks renew "$task" && return 0
    echo "claim on $task lapsed (a suspend or a very long gate); taking it back" >&2
    local -a scopes=()
    local s
    while IFS= read -r s; do
        [ -n "$s" ] && scopes+=(--scope "$s")
    done < <(git cat-file -p "refs/claims/$task" 2>/dev/null | jq -r '.scopes[]?' 2>/dev/null || true)
    # A legacy claim may have reserved nothing; re-claiming it deliberately is
    # what --unscoped is for, since a scopeless claim cannot be enforced.
    [ "${#scopes[@]}" -gt 0 ] || scopes=(--unscoped)
    "$susi_bin" tasks claim "$task" "${scopes[@]}"
}

# Renew the claims this agent holds. A size-l task runs past its lease, and an
# agent at work never reaches `finish`, where renewal otherwise happens; `sync`
# is the boundary a working agent does cross, so the lease is topped up here.
# Without it a lapsed lease is taken over and the same task is handed to a
# second agent while this one still holds the work.
# Evidence is a convention, not a gate: the ledger is what ranks providers by
# recorded outcomes, so an entry minted to satisfy a check is noise. Ask once,
# only for work that touched code, and only when no entry names the task.
evidence_prompt() {
    local task=$1 agent scopes
    command -v jq >/dev/null 2>&1 || return 0
    scopes=$(git cat-file -p "refs/claims/$task" 2>/dev/null | jq -r '.scopes[]?' 2>/dev/null || true)
    printf '%s\n' "$scopes" | grep -q '^crates/' || return 0
    grep -q "\"$task\"" .agents/evidence.json 2>/dev/null && return 0
    agent=$(git config --get susi.agent 2>/dev/null || echo AGENT)
    echo "ℹ️  $task changed crates/ but no evidence entry names it — if it changed a design decision"
    echo "   or produced a measurement, record it as EV-$agent-<n> in .agents/evidence.json."
    echo "   Not required: an entry minted to satisfy a check is worse than none."
}

# Run the workspace test gate, retrying the crates that failed once.
#
# Timing-sensitive tests assert that work overlaps in wall time, and a machine
# running five agents' builds cannot always give them that: the gate failed twice
# for one agent while the same crate passed when the machine was quiet. A
# plausible flake is not evidence of a regression, and stopping the loop for one
# costs a cycle and a live claim - but a failure that repeats is believed.
gate_tests() {
    local log crates c
    log=$(mktemp)
    if cargo test --workspace --locked 2>&1 | tee "$log"; then
        rm -f "$log"
        return 0
    fi
    # Anchor on cargo's own phrase: a loose pattern matched a stray '-p gapfix'
    # from the failure output and retired a package that does not exist.
    crates=$(grep -oE 'to rerun pass .-p [a-z0-9_-]+' "$log" 2>/dev/null | sed 's/.*-p //' | sort -u)
    rm -f "$log"
    [ -n "$crates" ] || return 1
    # Only a member of this workspace can be rerun: an end-to-end fixture runs a
    # deliberately failing cargo test inside its own throwaway workspace, and that
    # nested line would otherwise name a crate that does not exist here.
    members=$( { ls -d crates/*/ 2>/dev/null | xargs -n1 basename; sed -n 's/^name = "\(.*\)"/\1/p' Cargo.toml | head -1; } | sort -u)
    reran=0
    echo "the workspace run failed; retrying $(printf '%s ' $crates)up to 3 times - timing tests are load-sensitive, and this machine is never quiet during a swarm" >&2
    for c in $crates; do
        if ! grep -qx "$c" <<<"$members"; then
            echo "ignoring $c: not a package in this workspace" >&2
            continue
        fi
        reran=1
        passed=0
        for attempt in 1 2 3; do
            if cargo test --locked -p "$c"; then
                passed=1
                break
            fi
            [ "$attempt" -lt 3 ] && {
                echo "attempt $attempt failed for $c; waiting for the load to dip" >&2
                sleep 20
            }
        done
        if [ "$passed" -eq 0 ]; then
            echo "FAILED on 3 attempts: $c - a real failure, not a flake" >&2
            return 1
        fi
        echo "passed on retry: $c (attempt $attempt)" >&2
    done
    if [ "$reran" -eq 0 ]; then
        echo "the workspace tests failed and no crate named belongs to this workspace" >&2
        return 1
    fi
    return 0
}

renew_owned_claims() {
    local token id out
    # `git config --get` exits 1 when the key is unset, and under `pipefail`
    # that killed the whole sync for any worktree without an identity.
    token=$( { git config --get susi.agent 2>/dev/null || true; } | tr '[:lower:]' '[:upper:]' | tr -cd 'A-Z0-9')
    [ -n "$token" ] || return 0
    command -v jq >/dev/null 2>&1 || return 0
    out=$("$susi_bin" tasks list 2>/dev/null | jq -r --arg a "$token" \
        '.open[] | select(.claimed_by == $a) | .id' 2>/dev/null || true)
    for id in $out; do
        [ -n "$id" ] || continue
        "$susi_bin" tasks renew "$id" >/dev/null 2>&1 &&
            echo "claim $id renewed (this worktree's lease)"
    done
}

# The state of the pull request for this branch, when `gh` can answer. A closed
# PR will never merge, so waiting out the budget on one is pointless.
pr_state() {
    command -v gh >/dev/null 2>&1 || return 0
    gh pr view "$branch" --json state -q .state 2>/dev/null || true
}

# The conclusion of the branch-push run for this exact sha, when `gh` can answer:
# "completed/failure", "in_progress/pending", or empty when it cannot be read
# (no gh, no jq, no run yet) — unknown never stops the wait.
run_state() {
    command -v gh >/dev/null 2>&1 || return 0
    command -v jq >/dev/null 2>&1 || return 0
    gh run list --branch "$branch" --workflow Test --limit 20 \
        --json headSha,status,conclusion 2>/dev/null |
        jq -r --arg sha "$sha" '[.[] | select(.headSha == $sha)][0] | .status + "/" + (.conclusion // "pending")' 2>/dev/null || true
}

case "$action" in
sync) sync ;;
finish)
    task=${2:?task id required}
    renew_or_readopt
    sync
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    gate_tests
    # Anything still uncommitted - the verdict, the mastery test - must land
    # while the task is open: close moves the record to done/, and the commit hook
    # then refuses a commit citing a closed task. The scope check still refuses
    # anything outside the claim, so this cannot smuggle in unrelated work.
    if [ -n "$(git status --porcelain)" ]; then
        git add -A
        git commit -m "Record $task evidence before closure" -m "Task: $task"
    fi
    # Retain ownership through publication; close records acceptance in the tree.
    if [ -f ".agents/tasks/$task.json" ]; then
        "$susi_bin" tasks close "$task"
        evidence_prompt "$task"
        git add -- ".agents/tasks/$task.json" ".agents/tasks/done/$task.json"
        git commit -m "Close $task after acceptance" -m "Task: $task"
    fi
    [ -f ".agents/tasks/done/$task.json" ] || { echo 'No completed task record.' >&2; exit 1; }
    verified=$(git rev-parse HEAD)
    sync
    if [ "$verified" != "$(git rev-parse HEAD)" ]; then
        cargo fmt --all --check
        cargo clippy --workspace --all-targets --locked -- -D warnings
        gate_tests
    fi
    branch=$(git symbolic-ref --short HEAD)
    git push origin "HEAD:refs/heads/$branch"
    sha=$(git rev-parse HEAD)
    echo "Waiting for $sha to reach origin/main (Ctrl-C leaves work intact)."
    budget=${SUSI_FINISH_WAIT_MAX:-7200}
    poll=${SUSI_FINISH_POLL:-15}
    started=$(date +%s)
    ticks=0
    renew_at=$(( started + 900 ))
    until git fetch --quiet origin && git merge-base --is-ancestor "$sha" origin/main; do
        if [ "$(date +%s)" -ge "$renew_at" ]; then
            renew_or_readopt
            renew_at=$(( $(date +%s) + 900 ))
        fi
        # Another agent may merge first. Integrate it, rerun the gate and
        # publish a new head so the remote merge gate can reconsider this PR.
        if ! git merge-base --is-ancestor origin/main HEAD; then
            sync
            cargo fmt --all --check
            cargo clippy --workspace --all-targets --locked -- -D warnings
            gate_tests
            git push origin "HEAD:refs/heads/$branch"
            sha=$(git rev-parse HEAD)
        fi
        elapsed=$(( $(date +%s) - started ))
        # Without a budget this loop held the task claim forever on a branch
        # that could not merge — a red gate, or a PR the reconciler closes after
        # 7 idle days — and never told the agent.
        if [ "$elapsed" -ge "$budget" ]; then
            cat >&2 <<MSG
❌ $sha is still not on origin/main after ${elapsed}s (budget ${budget}s).
   Nothing is lost: the task is closed and the claim is retained, so no other
   agent starts it. Check the branch-push run and whether its pull request was
   closed, then run finish again (SUSI_FINISH_WAIT_MAX raises the budget).
MSG
            exit 1
        fi
        ticks=$(( ticks + 1 ))
        # A closed pull request will never merge, and neither will a run that has
        # already failed. Both used to be discovered only by waiting out the
        # whole budget — two hours by default — while the lease was renewed;
        # that is two hours in which the agent could have been fixing it.
        if [ $(( ticks % 4 )) -eq 0 ]; then
            case "$(pr_state)" in
            CLOSED)
                echo "❌ the pull request for $branch is closed; it will not merge." >&2
                echo "   The task is closed and the claim is retained — push a fix and reopen it, or release the claim." >&2
                exit 1
                ;;
            esac
            state=$(run_state)
            case "$state" in
            completed/failure | completed/timed_out | completed/startup_failure)
                cat >&2 <<MSG
❌ the branch-push run for $sha did not pass ($state).
   The task is closed and the claim is retained, so nothing is lost and no other
   agent will start it. Fix the failure, commit the repair, and run finish again
   (if the acceptance was already published, revert the close and fix under the
   same claim — citing a closed task is refused).
MSG
                exit 1
                ;;
            esac
        fi
        sleep "$poll"
    done
    sync
    "$susi_bin" tasks release "$task"
    echo 'Merged and synchronized; ready to claim the next task.'
    ;;
start-watch)
    nohup "$root/scripts/parallel-workflow.sh" watch >"$common/susi-primary-watch.log" 2>&1 </dev/null &
    ;;
watch)
    # This runs locally: hosted Actions cannot update a developer's filesystem.
    # A single watcher per clone, and a separate lock for primary tree updates.
    exec 8>"$common/susi-primary-watch.lock"
    flock -n 8 || { echo 'A primary sync watcher is already running.'; exit 0; }
    trap 'exit 0' INT TERM
    # A heartbeat, because the log stays empty while things go well: a stale
    # stamp is the only way `susi workflow check` can report a dead watcher.
    while true; do
        date +%s >"$common/susi-primary-watch.stamp" 2>/dev/null || true
        "$root/scripts/park-primary.sh" --sync-only
        sleep "${SUSI_SYNC_INTERVAL:-15}"
    done
    ;;
*) echo 'usage: parallel-workflow.sh sync | finish <task> | watch' >&2; exit 2 ;;
esac
