#!/usr/bin/env bash
# Safe boundaries for the per-task agent loop. Never merge into dirty worktrees.
set -euo pipefail
# Git for Windows' own usr/bin/sh.exe, started from PowerShell or the desktop
# app, has no /usr/bin on PATH: `dirname` below failed, posix-compat.sh (and
# its flock) was never loaded, and `sync` stopped at `flock 9`.
case "${OSTYPE:-}" in
msys* | cygwin*)
    case ":$PATH:" in
    *:/usr/bin:*) ;;
    *) PATH=/usr/bin:$PATH ;;
    esac
    ;;
esac
[ ! -f "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh" ] || . "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh"
root=$(git rev-parse --show-toplevel)
common=$(git rev-parse --path-format=absolute --git-common-dir)
action=${1:-sync}
lock="$common/susi-workflow.lock"
susi_bin=${SUSI_WORKFLOW_BIN:-susi}

sync() {
    # A cloud session's clone is the worker's own checkout: the mark says so
    # (scripts/cloud-session.sh exits 0 for a marked clone). The script's mere
    # presence says nothing — every clone has it.
    { [ -x "$root/scripts/cloud-session.sh" ] && "$root/scripts/cloud-session.sh"; } \
        || [ "$(git rev-parse --path-format=absolute --git-dir)" != "$common" ] \
        || { echo 'Use your own linked worktree.' >&2; return 1; }
    [ "$(git symbolic-ref -q --short HEAD)" != main ] || { echo 'Use your own branch.' >&2; return 1; }
    [ -z "$(git status --porcelain)" ] || { echo 'Commit or stash your work before syncing.' >&2; return 1; }
    # The clone-wide lock covers the fetch alone (two fetches into one .git
    # race on ref locks). It used to cover the merge, the primary's sync, the
    # watcher heal, the lease renewal, the owed-run report and the board, so
    # every agent in the clone queued behind every other agent's whole sync.
    exec 9>"$lock"
    flock 9
    git fetch --quiet origin
    flock -u 9
    exec 9>&-
    git merge --no-edit origin/main
    "$root/scripts/park-primary.sh" --sync-only || true
    # The watcher is a background process that dies with its session, and the
    # primary then stops converging until someone notices. Every agent crosses
    # this boundary, so the loop heals it here. Non-fatal either way.
    "$root/scripts/ensure-watcher.sh" || true
    # A working agent crosses this boundary but not `finish`, so top the lease up.
    renew_owned_claims
    # `finish` hands a pushed branch to the merge machinery without waiting
    # for its run; a run that then fails is reported here, where every agent
    # passes before its next claim (scripts/check-owed-runs.sh).
    "$root/scripts/check-owed-runs.sh" || true
    # The board is for people watching the swarm, not for the agent's next
    # step: it refreshes its cache in the background instead of holding the
    # sync for up to four checks of a minute each.
    if [ "${SUSI_SYNC_SKIP_BOARD:-0}" != 1 ]; then
        nohup "$root/scripts/swarm-status.sh" >/dev/null 2>&1 </dev/null &
    fi
    # The merge may have just brought this worktree the seeding: a target/
    # still linked to the clone's old shared one gets its own before anything
    # builds (seed_own_target). Outside the lock, which the copy would hold
    # for a minute.
    seed_own_target
}

# Every cargo the loop runs builds into this worktree's own target. A worktree
# whose session began before seeding existed may still have target/ linked to
# the clone's old shared one, where a sibling's build of a crate can stand in
# for this branch's: scripts/seed-target.sh replaces the link (the link only)
# and copies the snapshot in, in the foreground, before the first cargo. On a
# target that already holds a build it does nothing.
seed_own_target() {
    # "$BASH": this very bash. A bare `bash` can be WSL's on a Windows PATH.
    [ ! -f "$root/scripts/seed-target.sh" ] || "${BASH:-bash}" "$root/scripts/seed-target.sh" "$root" 9>&- || true
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
# The gate's verdict is a pure function of the diff it judges: the same
# diff-vs-main produces the same fmt/clippy/test outcome. finish used to pay
# the suite again on every re-invocation — a push rejection, a stale remote
# head whose merge only moves task JSON — even when the gate-relevant content
# was untouched. The stamp hashes the working tree's diff to origin/main with
# `.agents/` excluded (task bookkeeping is never gate input) plus the content
# of untracked code files, and on a hit the suite is skipped. A merge that
# pulls in real upstream changes moves the diff the other way and re-gates;
# SUSI_GATE_NOCACHE=1 forces a run regardless.
gate_stamp() {
    {
        git diff origin/main -- . ':(exclude).agents'
        git ls-files --others --exclude-standard -- . ':(exclude).agents' |
            while IFS= read -r f; do
                [ -f "$f" ] && git hash-object -- "$f"
            done
    } | git hash-object --stdin
}

# The gate's cargo builds into this worktree's own target, made so first
# (seed_own_target: a link to the old shared target is replaced), so plain cargo
# judges this branch's code: no other worktree builds there, and there is no
# shared target to lock or guard.
gate() {
    local okfile stamp pkgs lint sel lsel
    seed_own_target
    okfile="$(git rev-parse --git-dir)/susi-gate-ok"
    stamp=$(gate_stamp)
    if [ -z "${SUSI_GATE_NOCACHE:-}" ] && [ -f "$okfile" ] && [ "$(cat "$okfile")" = "$stamp" ]; then
        echo "gate: this exact diff already passed — skipping (SUSI_GATE_NOCACHE=1 forces a run)" >&2
        gate_pkgs=$("$root/scripts/ci-changed-crates.sh")
        return 0
    fi
    cargo fmt --all --check
    pkgs=$("$root/scripts/ci-changed-crates.sh")
    gate_pkgs=$pkgs
    if [ "${SUSI_LOCAL_GATE:-}" = full ]; then
        gate_pkgs=ALL
        cargo clippy --workspace --all-targets --locked -- -D warnings
        gate_tests --workspace
        echo "$stamp" >"$okfile"
        return
    fi
    if [ "$pkgs" = ALL ]; then
        # A workspace-wide change (lockfile, toolchain, cargo config): lint it
        # all, but leave the whole suite to main's post-merge run, which runs
        # it on every merge anyway and attributes a failure to the PR. Running
        # it here held the agent 15+ minutes per finish (SUSI_LOCAL_GATE=full
        # restores it). gate_pkgs stays empty: no acceptance is "covered".
        gate_pkgs=
        cargo clippy --workspace --all-targets --locked -- -D warnings
        echo "$stamp" >"$okfile"
        return
    fi
    if [ -z "$pkgs" ]; then
        echo 'gate: no crate touched by this diff — fmt is clean; the branch run proves the rest' >&2
        echo "$stamp" >"$okfile"
        return
    fi
    lint=$("$root/scripts/ci-changed-crates.sh" --dependents)
    lsel=$(printf ' -p %s' $lint)
    # The pre-push hook lints this exact set with this exact command line, so
    # its run is a cache hit rather than a second compile of other crates.
    cargo clippy --locked --all-targets $lsel -- -D warnings
    sel=$(printf ' -p %s' $pkgs)
    gate_tests $sel
    echo "$stamp" >"$okfile"
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
    # Teardown: the gate's tests auto-start the host daemon, whose leaf
    # children (`service-run`) would otherwise outlive this run and keep
    # `target\debug\susi.exe` locked on Windows. Runs on every exit path —
    # including a failing gate — scoped to this checkout's target dir.
    trap "\"$root/scripts/kill-leftover-service-runs.sh\" >/dev/null 2>&1 || true" EXIT
    # The default is not to wait: the branch-push run and the merge are the
    # machinery's (auto-merge, the reconciler), and nothing the agent does
    # after the push speeds either up. `--wait` (or SUSI_FINISH_WAIT=1) keeps
    # the old loop — poll the run, stop on red, hand off on green or at the
    # budget — for a session that wants the verdict in-line; `--no-wait` and
    # SUSI_FINISH_NOWAIT=1 are accepted and mean the default.
    # Environment first, then the flags: a flag on the command line always
    # wins over a variable left in a profile.
    wait=${SUSI_FINISH_WAIT:-0}
    [ "${SUSI_FINISH_NOWAIT:-0}" = 1 ] && wait=0
    for arg in "$@"; do
        case "$arg" in
        --wait) wait=1 ;;
        --no-wait) wait=0 ;;
        esac
    done
    export SUSI_SYNC_SKIP_BOARD=1
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
        [ "${SUSI_FINISH_REGATE:-0}" = 1 ] && SUSI_GATE_NOCACHE=1 gate
    fi
    branch=$(git symbolic-ref --short HEAD)
    git push origin "HEAD:refs/heads/$branch"
    sha=$(git rev-parse HEAD)
    if [ "$wait" != 1 ] || [ "${SUSI_FINISH_WAIT_MAX:-1800}" -le 0 ]; then
        # Never fall through into the wait below: a failed release used to
        # park the agent in a 30-minute poll it never asked for.
        if ! "$susi_bin" tasks release "$task" --force; then
            echo "❌ pushed $sha, but releasing $task's claim failed (see above); run: susi tasks release $task --force" >&2
            exit 1
        fi
        cat >&2 <<MSG
✅ pushed $sha and published the close receipt — the merge is the machinery's now (no wait).
   The close receipt on the remote keeps the debt until $sha is on
   origin/main (\`susi workflow check\` shows it under "merge published"), and
   a red run for it is reported at your next \`susi workflow sync\`.
   Claim the next task now; up to SUSI_MERGES_IN_FLIGHT (default 4) may be in flight.
   (\`susi workflow finish <id> --wait\` polls the run in-line instead.)
MSG
        exit 0
    fi
    echo "Waiting for $sha's run or merge (Ctrl-C leaves work intact)."
    # The wait's only value after push is catching a failure fast, while the
    # claim is still held and a fix is cheap. Once the run is green the merge
    # itself is machinery — auto-merge, or the reconciler resyncing a
    # green-but-behind head — and nothing the agent can do speeds it up, so it
    # hands off there rather than parking until the merge commit lands. The
    # budget handoff remains for a run that never reports (no gh, stuck queue).
    # A branch that cannot merge — closed PR, failed run — exits below keeping
    # the claim.
    budget=${SUSI_FINISH_WAIT_MAX:-1800}
    poll=${SUSI_FINISH_POLL:-10}
    started=$(date +%s)
    ticks=0
    renew_at=$(( started + 900 ))
    until git fetch --quiet origin && git merge-base --is-ancestor "$sha" origin/main; do
        if [ "$(date +%s)" -ge "$renew_at" ]; then
            renew_or_readopt
            renew_at=$(( $(date +%s) + 900 ))
        fi
        # Check the pushed head's state before integrating anything: a green
        # run for this exact sha means the merge machinery can finish it —
        # the reconciler resyncs green-but-behind heads itself — so repushing
        # now would only force a redundant new run.
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
        completed/success)
            # Green and still open: the merge machinery finishes this —
            # auto-merge on a current head, the reconciler on a behind
            # one. The agent's wait was only ever failure detection, and
            # that is answered now, so the claim is released here instead
            # of at the merge (or the budget). The close receipt keeps
            # the debt visible in \`susi workflow check\` until it lands.
            if "$susi_bin" tasks release "$task" --force; then
                cat >&2 <<MSG
✅ the run for $sha is green — handing the merge to the machinery.
   The close receipt on the remote keeps the debt until $sha is on
   origin/main (\`susi workflow check\` shows it under "merge published").
   Claim the next task now; up to SUSI_MERGES_IN_FLIGHT (default 4) may be in flight.
MSG
                exit 0
            fi
            ;;
        esac
        # Another agent may merge first. A head that is merely behind main
        # needs nothing from this agent: the merge machinery gates the merge
        # tree itself (auto-merge-pr.sh's inline gate). Re-pushing on every
        # advance of main cancelled this branch's own in-flight run each time
        # (test.yml cancels in progress per ref), so in a busy swarm a branch
        # whose run was slower than the gap between merges never went green.
        # Only a head that would *conflict* with main needs the agent: then it
        # syncs (and resolves) and publishes the new head.
        if ! git merge-base --is-ancestor origin/main HEAD \
            && ! git merge-tree --write-tree HEAD origin/main >/dev/null 2>&1; then
            sync
            [ "${SUSI_FINISH_REGATE:-0}" = 1 ] && SUSI_GATE_NOCACHE=1 gate
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
   claim the next task (up to SUSI_MERGES_IN_FLIGHT merges may be in flight).
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
    # Only "held" (1) means another watcher. Anything else is a lock that could
    # not be taken at all: a missing flock exits 127, which was read as a live
    # watcher, so none ran and the primary silently stopped converging.
    flock -n 8 || {
        rc=$?
        if [ "$rc" -ne 1 ]; then
            echo "watch: cannot take the watcher lock (flock exited $rc)" >&2
            exit "$rc"
        fi
        echo 'A primary sync watcher is already running.'
        exit 0
    }
    trap 'exit 0' INT TERM
    # The snapshot new worktrees copy their target from (scripts/seed-target.sh)
    # is rebuilt here, by its one writer: a refresh in its own checkout of
    # origin/main, started in the background at low priority, so the heartbeat
    # never waits on a build. It is asked every SUSI_SEED_CHECK_SECS (300) and
    # runs one at a time (it locks); it does nothing unless origin/main has
    # moved past the snapshot and the snapshot is older than
    # SUSI_SEED_REFRESH_SECS. SUSI_NO_SEED=1 turns it off.
    seed_next=0
    seed_tick() {
        [ "${SUSI_NO_SEED:-}" != 1 ] || return 0
        [ -f "$root/scripts/seed-target.sh" ] || return 0
        [ "$SECONDS" -ge "$seed_next" ] || return 0
        seed_next=$((SECONDS + ${SUSI_SEED_CHECK_SECS:-300}))
        local nice=
        ! command -v nice >/dev/null 2>&1 || nice="nice -n 19"
        # Not the watcher's lock (fd 8): a refresh that outlived this watcher
        # would hold it and keep the next one from starting.
        # shellcheck disable=SC2086 # an empty $nice is no word at all
        nohup $nice "${BASH:-bash}" "$root/scripts/seed-target.sh" --refresh \
            --log "$common/susi-seed-refresh.log" >/dev/null 2>&1 </dev/null 8>&- &
    }
    # A heartbeat, because the log stays empty while things go well: a stale
    # stamp is the only way `susi workflow check` can report a dead watcher.
    while true; do
        date +%s >"$common/susi-primary-watch.stamp" 2>/dev/null || true
        "$root/scripts/park-primary.sh" --sync-only || true
        seed_tick || true
        sleep "${SUSI_SYNC_INTERVAL:-15}"
    done
    ;;
*) echo 'usage: parallel-workflow.sh sync | finish <task> | watch' >&2; exit 2 ;;
esac
