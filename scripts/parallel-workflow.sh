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

# Run the changed crates' tests: ONCE, hermetically, with per-test retries.
#
# This is the only place a branch's tests run before it merges - the branch-push
# run compiles and lints, it does not execute tests - so it does what two remote
# steps used to: the hermetic check (Mandate 52: a throwaway HOME/XDG/SUSI_HOME,
# and a failure if a test writes into it) and the run itself are one pass of
# scripts/check-hermetic-tests.sh under cargo-nextest. nextest retries a failing
# *test* alone (SUSI_HERMETIC_RETRIES, default 2) and reports the ones that pass
# on retry as flaky, which replaces the per-crate rerun below; doc-tests, which
# nextest does not run, follow for the crates that have a library. A host without
# nextest keeps the cargo-test path.
gate_tests() {
    if cargo nextest --version >/dev/null 2>&1; then
        SUSI_HERMETIC_RUNNER=nextest "$root/scripts/check-hermetic-tests.sh" "$@"
        return
    fi
    gate_tests_cargo "$@"
}

# The cargo-test path, retrying the crates that failed once.
#
# Timing-sensitive tests assert that work overlaps in wall time, and a machine
# running five agents' builds cannot always give them that: the gate failed twice
# for one agent while the same crate passed when the machine was quiet. A
# plausible flake is not evidence of a regression, and stopping the loop for one
# costs a cycle and a live claim - but a failure that repeats is believed.
gate_tests_cargo() {
    local log crates c
    log=$(mktemp)
    if cargo test --locked "$@" 2>&1 | tee "$log"; then
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

# The local gate proves what the diff can break, not the whole workspace:
# `ci-changed-crates.sh` names the packages the branch touches — the same
# scoping the branch-push CI gate applies — so a task that changes one crate
# checks and tests only that crate. Two sets, because a break and a test failure
# have different reach: the tests that can change are the changed crates', while
# what can stop *compiling* also includes their dependents (`--dependents`), so
# clippy takes the wider set and the tests the narrower. The full suite is kept
# for workspace-wide inputs (lockfile, toolchain, root build inputs) or when
# detection cannot tell, and `SUSI_LOCAL_GATE=full` forces it. A docs/CI-only
# diff has nothing local to prove beyond fmt: the branch-push run gates the
# merge, and main re-runs the full suite after it.
gate() {
    local pkgs lint sel lsel
    cargo fmt --all --check
    pkgs=$("$root/scripts/ci-changed-crates.sh")
    gate_pkgs=$pkgs
    if [ "$pkgs" = ALL ] || [ "${SUSI_LOCAL_GATE:-}" = full ]; then
        gate_pkgs=ALL
        cargo clippy --workspace --all-targets --locked -- -D warnings
        gate_tests --workspace
        return
    fi
    if [ -z "$pkgs" ]; then
        echo 'gate: no crate touched by this diff — fmt is clean; the branch run proves the rest' >&2
        return
    fi
    lint=$("$root/scripts/ci-changed-crates.sh" --dependents)
    lsel=$(printf ' -p %s' $lint)
    # The pre-push hook lints this exact set with this exact command line, so
    # its run is a cache hit rather than a second compile of other crates.
    cargo clippy --locked --all-targets $lsel -- -D warnings
    sel=$(printf ' -p %s' $pkgs)
    gate_tests $sel
}

# Whether the gate run above already covered a task's acceptance command. Only
# cargo test/nextest acceptances can be covered — a scripts/ checker asserts
# things no cargo suite does — and only when the gate's package selection
# contains the package the command tests (ALL covers everything).
acceptance_covered() {
    local cmd=$1 pkg
    case "$cmd" in
    "cargo test "*|"cargo nextest run "*) ;;
    *) return 1 ;;
    esac
    [ "$gate_pkgs" = ALL ] && return 0
    case "$cmd" in
    *--workspace*|*--all*) return 1 ;; # needs the whole suite, not a subset
    esac
    # The package under test: an explicit -p/--package wins; a bare
    # `cargo test --test x` at the root exercises the root package.
    pkg=$(printf '%s\n' "$cmd" | grep -oE '(-p |--package[ =])[a-zA-Z0-9_-]+' | head -1 | grep -oE '[a-zA-Z0-9_-]+$')
    [ -n "$pkg" ] || pkg=$(sed -n 's/^name = "\(.*\)"/\1/p' Cargo.toml | head -1)
    [ -n "$pkg" ] || return 1
    printf '%s\n' $gate_pkgs | grep -qx "$pkg"
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
    gate
    # Anything still uncommitted - the verdict, the mastery test - must land
    # while the task is open: close moves the record to done/, and the commit hook
    # then refuses a commit citing a closed task. The scope check still refuses
    # anything outside the claim, so this cannot smuggle in unrelated work.
    if [ -n "$(git status --porcelain)" ]; then
        git add -A
        git commit -m "Record $task evidence before closure" -m "Task: $task"
    fi
    # Retain ownership through publication; close records acceptance in the tree.
    # The gate just ran this task's tests — when it provably covered the
    # acceptance command (same package or the whole workspace), tell close the
    # run happened so it does not re-execute the same cargo test a second time.
    if [ -f ".agents/tasks/$task.json" ]; then
        accept_cmd=$(jq -r '.accept.cmd | join(" ")' ".agents/tasks/$task.json" 2>/dev/null || true)
        if [ -n "$accept_cmd" ] && acceptance_covered "$accept_cmd"; then
            echo "gate: acceptance \`$accept_cmd\` already ran in the gate — close will not re-run it" >&2
            SUSI_ACCEPTANCE_COVERED="$task@$root" "$susi_bin" tasks close "$task"
        else
            "$susi_bin" tasks close "$task"
        fi
        evidence_prompt "$task"
        git add -- ".agents/tasks/$task.json" ".agents/tasks/done/$task.json"
        git commit -m "Close $task after acceptance" -m "Task: $task"
    fi
    [ -f ".agents/tasks/done/$task.json" ] || { echo 'No completed task record.' >&2; exit 1; }
    verified=$(git rev-parse HEAD)
    sync
    if [ "$verified" != "$(git rev-parse HEAD)" ]; then
        # The head moved because another merge landed while the gate ran.
        # Repushing the merged head starts a fresh branch run, and nothing
        # merges without a green run on that exact sha — re-running the local
        # gate here multiplied the suite by every merge in the window.
        # SUSI_FINISH_REGATE=1 restores the old belt-and-suspenders check.
        [ "${SUSI_FINISH_REGATE:-0}" = 1 ] && gate
    fi
    branch=$(git symbolic-ref --short HEAD)
    git push origin "HEAD:refs/heads/$branch"
    sha=$(git rev-parse HEAD)
    echo "Waiting for $sha to reach origin/main (Ctrl-C leaves work intact)."
    # A green head that is merely queued is the merge machinery's problem, not
    # this agent's — so the budget is a handoff, not a wait-out: past it the
    # claim is released (the close receipt keeps the debt) and the agent may
    # take the next task. A branch that cannot merge — closed PR, failed run —
    # exits below long before this point, keeping the claim.
    budget=${SUSI_FINISH_WAIT_MAX:-1800}
    poll=${SUSI_FINISH_POLL:-15}
    started=$(date +%s)
    ticks=0
    renew_at=$(( started + 900 ))
    until git fetch --quiet origin && git merge-base --is-ancestor "$sha" origin/main; do
        if [ "$(date +%s)" -ge "$renew_at" ]; then
            renew_or_readopt
            renew_at=$(( $(date +%s) + 900 ))
        fi
        # Another agent may merge first. Integrate it and publish a new head
        # so the remote merge gate can reconsider this PR; the new head's own
        # branch run proves it — that is what the remote gate is for.
        if ! git merge-base --is-ancestor origin/main HEAD; then
            sync
            [ "${SUSI_FINISH_REGATE:-0}" = 1 ] && gate
            git push origin "HEAD:refs/heads/$branch"
            sha=$(git rev-parse HEAD)
        fi
        elapsed=$(( $(date +%s) - started ))
        # Past the budget the branch is green but still queued. The close
        # receipt on the shared remote already keeps the debt — one merge in
        # flight is the allowed pipeline — so the claim is freed for the next
        # task instead of holding the agent hostage to queue length.
        if [ "$elapsed" -ge "$budget" ]; then
            if "$susi_bin" tasks release "$task" --force; then
                cat >&2 <<MSG
⏳ $sha is still not on origin/main after ${elapsed}s — the merge is queued,
   not failed, so the claim is released and the merge machinery owns the rest.
   The close receipt on the remote keeps the debt: `susi workflow check` shows
   it under "merge published", and it clears when the merge lands. You may
   claim the next task; a second outstanding close would be refused.
MSG
                exit 0
            fi
            cat >&2 <<MSG
❌ $sha is still not on origin/main after ${elapsed}s (budget ${budget}s), and
   releasing the claim failed — the claim is retained, so no other agent starts
   the task. Check the branch-push run and whether its pull request was closed,
   then run finish again (SUSI_FINISH_WAIT_MAX raises the budget).
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
                echo "   The task is closed and its receipt keeps the claim (and this agent) held." >&2
                echo "   Fix it and reopen, or give it up deliberately and on the record:" >&2
                echo "   susi tasks release $task --abandon <reason>" >&2
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
   same claim — citing a closed task is refused). To give the task up instead:
   susi tasks release $task --abandon <reason>
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
