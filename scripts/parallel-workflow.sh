#!/usr/bin/env bash
# Safe boundaries for the per-task agent loop. Never merge into dirty worktrees.
set -euo pipefail
# A mid-compile cache disconnect must use the same local compiler rather than
# discard a gate. Explicit 0 retains sccache's strict server-error behavior.
export SCCACHE_IGNORE_SERVER_IO_ERROR=${SCCACHE_IGNORE_SERVER_IO_ERROR:-1}
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
if [ -f "$common/config" ] && ! grep -q "auto = 0" "$common/config" 2>/dev/null; then
    git config gc.auto 0 2>/dev/null || true
    git config maintenance.auto false 2>/dev/null || true
fi

sync_fetch_origin() {
    local remote=${SUSI_TASK_REMOTE:-origin}
    [[ "$remote" =~ [^[:space:]] ]] || remote=origin
    if [ "${SUSI_SYNC_IN_FINISH:-0}" != 1 ] && [ "$remote" = origin ]; then
        git fetch --quiet --prune origin \
            "+refs/heads/*:refs/remotes/origin/*" \
            "+refs/claims/*:refs/claims/*" \
            "+refs/claim-agents/*:refs/claim-agents/*" \
            "+refs/closed/*:refs/closed/*"
    else
        git fetch --quiet origin
    fi
}

prefetch_finish_refs() {
    local remote=${SUSI_TASK_REMOTE:-origin}
    [[ "$remote" =~ [^[:space:]] ]] || remote=origin
    [ "$remote" = origin ] || return 0
    SUSI_PREFETCH_LOCAL_CLAIM_OID=$(git rev-parse --verify --quiet "refs/claims/$task" 2>/dev/null || true)
    SUSI_PREFETCH_LOCAL_CLAIM_BODY=$(git cat-file -p "refs/claims/$task" 2>/dev/null || true)
    local fetch_seq="$common/susi-fetch.seq"
    exec 9>"$lock"
    flock 9
    if git fetch --quiet --prune origin \
        "+refs/heads/*:refs/remotes/origin/*" \
        "+refs/claims/*:refs/claims/*" \
        "+refs/claim-agents/*:refs/claim-agents/*" \
        "+refs/closed/*:refs/closed/*"; then
        printf '%s\n' "$(( $(cat "$fetch_seq" 2>/dev/null || echo 0) + 1 ))" >"$fetch_seq" 2>/dev/null || true
        SUSI_FINISH_ORIGIN_FETCHED=1
        export SUSI_CLOSE_QUEUE_SYNCED="$task@$root"
    else
        unset SUSI_PREFETCH_LOCAL_CLAIM_OID SUSI_PREFETCH_LOCAL_CLAIM_BODY
    fi
    flock -u 9
    exec 9>&-
}

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
    # race on ref locks). When several agents enter `sync` together, the
    # holder's fetch updates `refs/remotes/origin/*` for the whole clone:
    # waiters that observe `susi-fetch.seq` advance while queued reuse that
    # completed fetch instead of running N back-to-back remote round-trips.
    if [ "${SUSI_FINISH_ORIGIN_FETCHED:-0}" = 1 ]; then
        SUSI_FINISH_ORIGIN_FETCHED=0
    else
        local fetch_seq="$common/susi-fetch.seq"
        exec 9>"$lock"
        if flock -n 9; then
            sync_fetch_origin
            printf '%s\n' "$(( $(cat "$fetch_seq" 2>/dev/null || echo 0) + 1 ))" >"$fetch_seq" 2>/dev/null || true
        else
            local before_seq
            before_seq=$(cat "$fetch_seq" 2>/dev/null || echo 0)
            flock 9
            if [ "$(cat "$fetch_seq" 2>/dev/null || echo 0)" = "$before_seq" ]; then
                sync_fetch_origin
                printf '%s\n' "$(( before_seq + 1 ))" >"$fetch_seq" 2>/dev/null || true
            fi
        fi
        flock -u 9
        exec 9>&-
    fi
    git merge --no-edit origin/main
    # The watcher is a background process that dies with its session, and the
    # primary then stops converging until someone notices. Every agent crosses
    # this boundary, so the loop heals it here. Non-fatal either way.
    # A working agent crosses this boundary but not `finish`, so top the lease up.
    # Inside `finish`, `renew_or_readopt` already renewed on entry and `release`
    # frees the claim at the end, while `check-owed-runs.sh` is for the boundary
    # before the next claim — skipping both inside `finish`'s two `sync` calls
    # saves 8-10 redundant remote round-trips per finish.
    if [ "${SUSI_SYNC_IN_FINISH:-0}" != 1 ]; then
        maintain_primary &
        local pid_primary=$!
        SUSI_REMOTE_CLAIM_FETCHED=1 renew_owned_claims &
        local pid_renew=$!
        SUSI_OWED_REFS_FETCHED=1 "$root/scripts/check-owed-runs.sh" &
        local pid_owed=$!
        wait "$pid_primary" 2>/dev/null || true
        wait "$pid_renew" 2>/dev/null || true
        wait "$pid_owed" 2>/dev/null || true
    fi
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
    # Finish's gate seeds immediately before Cargo. Other sync callers still
    # need the foreground seed here.
    [ "${SUSI_SYNC_IN_FINISH:-0}" = 1 ] || seed_own_target
}

maintain_primary() {
    SUSI_PARK_REFS_FETCHED=1 "$root/scripts/park-primary.sh" --sync-only || true
    "$root/scripts/ensure-watcher.sh" || true
}

# Every cargo the loop runs builds into this worktree's own target. A worktree
# whose session began before seeding existed may still have target/ linked to
# the clone's old shared one, where a sibling's build of a crate can stand in
# for this branch's: scripts/seed-target.sh replaces the link (the link only)
# and copies the snapshot in, in the foreground, before the first cargo. On a
# target that already holds a build it does nothing.
seed_own_target() {
    if [ -d "$root/target" ] && [ ! -L "$root/target" ] && { [ -f "$root/target/.susi-seeded" ] || [ -f "$root/target/CACHEDIR.TAG" ]; }; then
        return 0
    fi
    # "$BASH": this very bash. A bare `bash` can be WSL's on a Windows PATH.
    [ ! -f "$root/scripts/seed-target.sh" ] || "${BASH:-bash}" "$root/scripts/seed-target.sh" "$root" 9>&- || true
}

# Keep the claim alive across a long wait. `renew` refuses an expired lease, and
# under `set -e` that used to abort finish mid-wait — the PR stranded, the task
# no longer owned by anyone, and its work free for another agent to redo. A
# lapsed lease is recoverable: take the same task back, keeping its scopes.
renew_or_readopt() {
    # A fresh claim normally has hours left. Extending it again on every finish
    # costs a ref fetch, ownership lookup and push. Close and pre-push still
    # validate remote ownership; this only avoids an unnecessary lease write.
    local claim agent branch local_oid remote remote_oid remote_ref prefetched=0
    if [ -n "${SUSI_PREFETCH_LOCAL_CLAIM_BODY+x}" ]; then
        claim=$SUSI_PREFETCH_LOCAL_CLAIM_BODY
        local_oid=${SUSI_PREFETCH_LOCAL_CLAIM_OID:-}
        prefetched=1
        unset SUSI_PREFETCH_LOCAL_CLAIM_BODY SUSI_PREFETCH_LOCAL_CLAIM_OID
    else
        claim=$(git cat-file -p "refs/claims/$task" 2>/dev/null || true)
    fi
    agent=${SUSI_AGENT:-$(git config --get susi.agent || true)}
    branch=$(git symbolic-ref -q --short HEAD || true)
    if [ -n "$agent" ] && jq -e --arg agent "$agent" --arg branch "$branch" \
        --argjson minimum "$(( $(date +%s) + 900 ))" \
        '.agent == $agent and .branch == $branch and (.lease_until_unix // 0) > $minimum' \
        <<<"$claim" >/dev/null 2>&1; then
        # A local ref may be stale even when its embedded lease is healthy:
        # another checkout can have expired or reassigned it on the remote.
        # Avoid a renewal only when the remote still advertises this exact blob.
        [ "$prefetched" = 1 ] || local_oid=$(git rev-parse --verify --quiet "refs/claims/$task" 2>/dev/null || true)
        remote=${SUSI_TASK_REMOTE:-origin}
        [[ "$remote" =~ [^[:space:]] ]] || remote=origin
        if { [ "${SUSI_REMOTE_CLAIM_FETCHED:-0}" = 1 ] || [ "$prefetched" = 1 ]; } && [ "$remote" = origin ]; then
            remote_oid=$(git rev-parse --verify --quiet "refs/claims/$task" 2>/dev/null || true)
            remote_ref=${remote_oid:+"$remote_oid"$'\t'"refs/claims/$task"}
        else
            remote_ref=$(git ls-remote --refs "$remote" "refs/claims/$task" 2>/dev/null || true)
        fi
        remote_ref=${remote_ref%$'\r'}
        remote_oid=${remote_ref%%$'\t'*}
        if [ -n "$local_oid" ] && [ "$remote_oid" = "$local_oid" ] \
            && [ "$remote_ref" = "$local_oid"$'\t'"refs/claims/$task" ]; then
            return 0
        fi
    fi
    "$susi_bin" tasks renew "$task" && return 0
    echo "claim on $task lapsed (a suspend or a very long gate); taking it back" >&2
    local -a scopes=()
    local s
    while IFS= read -r s; do
        s=${s%$'\r'} # Native Windows jq may write CRLF.
        [ -n "$s" ] && scopes+=(--scope "$s")
    done < <({ [ -n "$claim" ] && printf '%s\n' "$claim" || git cat-file -p "refs/claims/$task" 2>/dev/null; } | jq -r '.scopes[]?' 2>/dev/null || true)
    # A legacy claim may have reserved nothing; re-claiming it deliberately is
    # what --unscoped is for, since a scopeless claim cannot be enforced.
    [ "${#scopes[@]}" -gt 0 ] || scopes=(--unscoped)
    "$susi_bin" tasks claim "$task" "${scopes[@]}"
}

# Use the shell's clock so observing the stages adds no subprocess launches.
timed_step() {
    local label=$1 started=$SECONDS
    shift
    "$@"
    local status=$?
    echo "workflow: $label took $((SECONDS - started))s" >&2
    return "$status"
}

# Renew the claims this agent holds. A size-l task runs past its lease, and an
# agent at work never reaches `finish`, where renewal otherwise happens; `sync`
# is the boundary a working agent does cross, so the lease is topped up here.
# Without it a lapsed lease is taken over and the same task is handed to a
# second agent while this one still holds the work.
renew_owned_claims() {
    local agent branch body task refs
    agent=${SUSI_AGENT:-$(git config --get susi.agent 2>/dev/null || true)}
    agent=$(printf '%s' "$agent" | tr '[:lower:]' '[:upper:]' | tr -cd 'A-Z0-9')
    [ -n "$agent" ] || return 0
    branch=$(git symbolic-ref -q --short HEAD || true)
    body=$(git cat-file -p "refs/claim-agents/$agent" 2>/dev/null || true)
    if [ -z "$body" ]; then
        # Legacy leases predate the agent index. Read their blobs as one batch.
        refs=$(git for-each-ref --format='%(objectname)' refs/claims/)
        body=$(printf '%s\n' "$refs" | git cat-file --batch 2>/dev/null |
            awk 'NF && !/^[0-9a-f]+ blob [0-9]+$/ { print }')
    fi
    while IFS= read -r task; do
        [ -n "$task" ] || continue
        git cat-file -e "origin/main:.agents/tasks/done/$task.json" 2>/dev/null && continue
        renew_or_readopt
    done < <(jq -r --arg agent "$agent" --arg branch "$branch" \
        'select(.agent == $agent and (.branch == null or .branch == $branch)) | .task' <<<"$body" 2>/dev/null || true)
}

# The push hook already fetches and checks main. Avoid a second full sync on
# the common path; if main advanced during the gate, integrate the hook's fresh
# snapshot and retry once. Other push failures retain their original error.
push_closed_branch() {
    local branch=$1 attempt before status
    released_with_push=0
    for attempt in 1 2; do
        before=$(git rev-parse HEAD)
        status=75
        if [ "${wait:-0}" != 1 ] && [ -n "${task:-}" ] && [ -f "$root/scripts/publish-task-branch.sh" ]; then
            if "$root/scripts/publish-task-branch.sh" "$task" "$branch"; then
                released_with_push=1
                return 0
            else
                status=$?
            fi
        fi
        # Relay transports and older standalone copies retain the separate
        # publication/release path. A failed atomic transaction never falls back.
        if [ "$status" = 75 ]; then
            if git push origin "HEAD:refs/heads/$branch"; then
                return 0
            fi
        fi
        if [ "$attempt" = 2 ] || [ "$before" != "$(git rev-parse HEAD)" ] \
            || git merge-base --is-ancestor origin/main HEAD; then
            return 1
        fi
        echo 'workflow: main advanced; integrating the fetched snapshot and retrying push' >&2
        if ! git merge --no-edit origin/main; then
            return 1
        fi
        if [ "${SUSI_FINISH_REGATE:-0}" = 1 ]; then
            SUSI_GATE_NOCACHE=1 gate
        fi
    done
}

finish_cleanup() {
    "$root/scripts/kill-leftover-service-runs.sh" >/dev/null 2>&1 || true
    # Updating another checkout and healing its watcher are housekeeping. They
    # must not extend the validated task's critical path or hold its streams.
    (trap - EXIT; trap '' HUP; maintain_primary) >>"$common/susi-primary-maintenance.log" 2>&1 </dev/null &
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
# The gate's proof is bound to its source inputs and selected scope. finish used to pay
# the suite again on every re-invocation — a push rejection, a stale remote
# head whose merge only moves task JSON — even when the gate-relevant content
# was untouched. The shared stamp includes all indexed sources, working edits,
# untracked inputs and scope, excluding only task bookkeeping. Changes pulled
# from main invalidate it even when they leave this branch's diff unchanged;
# SUSI_GATE_NOCACHE=1 forces a run regardless.
gate_stamp() {
    if [ -x "$root/scripts/gate-input-stamp.sh" ]; then
        "$root/scripts/gate-input-stamp.sh"
    else
        # Older standalone copies still run every check, without proof reuse.
        printf 'uncached\n'
    fi
}

gate_format() {
    local stamp okfile
    if [ ! -x "$root/scripts/gate-input-stamp.sh" ]; then
        cargo fmt --all --check
        return
    fi
    stamp=$("$root/scripts/gate-input-stamp.sh" --format) || return 1
    okfile="$(git rev-parse --git-dir)/susi-fmt-ok"
    if [ "$stamp" != uncached ] && [ -z "${SUSI_GATE_NOCACHE:-}" ] && [ -f "$okfile" ] && [ "$(cat "$okfile")" = "$stamp" ]; then
        echo 'format: this source and formatter already passed' >&2
        return 0
    fi
    cargo fmt --all --check || return 1
    printf '%s\n' "$stamp" >"$okfile"
}

# Refuse a diff that deletes tests.
#
# An engine worker satisfied a task's acceptance by overwriting
# tests/gmcp_requirements.rs: one new #[test] kept, 75 requirement tests and
# 13,764 lines gone. The acceptance passed (its one test ran), the gate passed
# (the file compiled and linted), and the branch was pushed for auto-merge; no
# CI job counts tests, so the merge would have been green. The gate now counts
# the test attributes (`#[test]`, `#[tokio::test]`) in every .rs file this
# branch changed against the merge base with origin/main, and refuses the diff
# when the total fell: a test moved between changed files keeps the total, a
# deleted one does not. Deleting a test on purpose is a decision an operator
# makes in the open: SUSI_GATE_ALLOW_TEST_REMOVAL=1 lets the diff through, and
# the refusal names the files so the override is never a guess.
gate_tests_kept() {
    local mb file before after total_before=0 total_after=0 dropped=
    [ -n "${SUSI_GATE_ALLOW_TEST_REMOVAL:-}" ] && return 0
    mb=$(git merge-base origin/main HEAD 2>/dev/null) || return 0
    while IFS= read -r file; do
        [ -n "$file" ] || continue
        before=$(git show "$mb:$file" 2>/dev/null | grep -cE '^[[:space:]]*#\[(tokio::)?test([]\(]|$)' || true)
        if [ -f "$file" ]; then
            after=$(grep -cE '^[[:space:]]*#\[(tokio::)?test([]\(]|$)' "$file" || true)
        else
            after=0
        fi
        before=${before:-0}
        after=${after:-0}
        total_before=$((total_before + before))
        total_after=$((total_after + after))
        [ "$after" -lt "$before" ] && dropped="$dropped
   $file: $before test(s) at the merge base, $after now"
    done < <(git -c diff.relative=false -c core.quotePath=false -c color.ui=never \
        diff --no-renames --no-color --name-only "$mb" HEAD -- '*.rs' 2>/dev/null | sort -u)
    [ "$total_after" -lt "$total_before" ] || return 0
    echo "❌ gate: this diff deletes tests — $total_before test(s) in the changed files at the merge base, $total_after now:$dropped" >&2
    echo "   A change that makes an acceptance pass by removing other tests is refused. Deleting a test on purpose: SUSI_GATE_ALLOW_TEST_REMOVAL=1 susi workflow finish <id>" >&2
    return 1
}

# The gate's cargo builds into this worktree's own target, made so first
# (seed_own_target: a link to the old shared target is replaced), so plain cargo
# judges this branch's code: no other worktree builds there, and there is no
# shared target to lock or guard.
resolve_gate_plan() {
    local plan kind value
    gate_pkgs= gate_lint= gate_root_tests= gate_rootpkg=
    plan=$("$root/scripts/ci-changed-crates.sh" --plan)
    if [ "$plan" = ALL ]; then
        gate_pkgs=ALL
        gate_lint=ALL
        return
    fi
    while IFS=: read -r kind value; do
        case "$kind" in
        test) gate_pkgs+="$value"$'\n' ;;
        lint) gate_lint+="$value"$'\n' ;;
        root-test) gate_root_tests+="$value"$'\n' ;;
        root-package) gate_rootpkg=$value ;;
        "") ;;
        *) gate_pkgs=ALL; gate_lint=ALL; gate_root_tests=; return ;;
        esac
    done <<<"$plan"
    # Match command-substitution output used by existing package comparisons.
    gate_pkgs=${gate_pkgs%$'\n'}
    gate_lint=${gate_lint%$'\n'}
    gate_root_tests=${gate_root_tests%$'\n'}
}

gate() {
    local okfile stamp pkgs lint sel lsel rootpkg root_tests other_pkgs
    command -v timed_step >/dev/null 2>&1 || timed_step() { shift; "$@"; }
    # Before the cache: a stamp written before this check existed must not
    # wave a test-deleting diff through.
    timed_step test-integrity gate_tests_kept || return 1
    unset SUSI_BRANCH_CI_HANDOFF || true
    if [ "${SUSI_FINISH_REMOTE_GATE:-0}" = 1 ] &&
       [ -f "$root/scripts/ci-branch-handoff.sh" ] &&
       "${BASH:-bash}" "$root/scripts/ci-branch-handoff.sh"; then
        command -v gate_format >/dev/null 2>&1 || gate_format() { cargo fmt --all --check; }
        timed_step format gate_format || return 1
        # No local suite ran: close must execute the task acceptance itself.
        gate_pkgs= gate_lint= gate_root_tests= gate_rootpkg=
        unset SUSI_ACCEPTANCE_COVERED || true
        export SUSI_BRANCH_CI_HANDOFF=1
        echo 'gate: affected tests and clippy will run in the required GitHub branch check; task acceptance still runs locally' >&2
        return 0
    fi
    timed_step target seed_own_target
    timed_step scope resolve_gate_plan
    okfile="$(git rev-parse --git-dir)/susi-gate-ok"
    stamp=$(gate_stamp) || return 1
    if [ "$stamp" != uncached ] && [ -z "${SUSI_GATE_NOCACHE:-}" ] && [ -f "$okfile" ] && [ "$(cat "$okfile")" = "$stamp" ]; then
        echo "gate: this exact diff already passed — skipping (SUSI_GATE_NOCACHE=1 forces a run)" >&2
        return 0
    fi
    command -v gate_format >/dev/null 2>&1 || gate_format() { cargo fmt --all --check; }
    timed_step format gate_format
    pkgs=$gate_pkgs
    if [ "${SUSI_LOCAL_GATE:-}" = full ]; then
        gate_pkgs=ALL
        timed_step clippy cargo clippy --workspace --all-targets --locked -- -D warnings
        timed_step tests gate_tests --workspace
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
        timed_step clippy cargo clippy --workspace --all-targets --locked -- -D warnings
        echo "$stamp" >"$okfile"
        return
    fi
    if [ -z "$pkgs" ]; then
        echo 'gate: no crate touched by this diff — fmt is clean; the branch run proves the rest' >&2
        echo "$stamp" >"$okfile"
        return
    fi
    lint=$gate_lint
    lsel=$(printf ' -p %s' $lint)
    # The pre-push hook lints this exact set with this exact command line, so
    # its run is a cache hit rather than a second compile of other crates.
    rootpkg=$gate_rootpkg
    root_test_targets=$gate_root_tests
    local -a clippy_args=()
    if [ "$pkgs" = "$rootpkg" ] && [ -n "$root_test_targets" ]; then
        local -a test_args=()
        while IFS= read -r target; do
            [ -n "$target" ] && test_args+=(--test "$target")
        done <<<"$root_test_targets"
        clippy_args=(--locked -p "$rootpkg" "${test_args[@]}" -- -D warnings)
    else
        clippy_args=(--locked --all-targets $lsel -- -D warnings)
    fi
    # The lint and the tests overlap: see gate_lint_and_test. The test steps
    # run in a subshell of this function, so they read its selection as before.
    gate_lint_and_test "${clippy_args[@]}" || return 1
    echo "$stamp" >"$okfile"
}

# The gate's test steps, for the selection resolve_gate_plan left in the
# gate_* variables: the changed crates' suites, with the root package narrowed
# to its touched `tests/<name>.rs` binaries when the diff allows.
gate_test_steps() {
    local pkgs=$gate_pkgs rootpkg=$gate_rootpkg root_tests other_pkgs sel
    if [ -n "$gate_root_tests" ]; then
        root_tests=$gate_root_tests
        other_pkgs=$(printf '%s\n' $pkgs | grep -vx "$rootpkg" || true)
        if [ -n "$other_pkgs" ]; then
            timed_step tests gate_tests $(printf ' -p %s' $other_pkgs) || return 1
        fi
        timed_step tests gate_tests -p "$rootpkg" $(printf ' --test %s' $root_tests)
    elif [ -n "$rootpkg" ] && printf '%s\n' $pkgs | grep -qx "$rootpkg"; then
        other_pkgs=$(printf '%s\n' $pkgs | grep -vx "$rootpkg" || true)
        if [ -n "$other_pkgs" ]; then
            timed_step tests gate_tests $(printf ' -p %s' $other_pkgs) || return 1
        fi
        timed_step tests gate_tests -p "$rootpkg" --lib --bins
    else
        sel=$(printf ' -p %s' $pkgs)
        timed_step tests gate_tests $sel
    fi
}

# Clippy and the tests, overlapped. Both start with a cargo build into the same
# target, and cargo's lock on the build directory serializes those; but the
# test *run* holds no lock, so once the test binaries are built clippy compiles
# while they execute, instead of after. The tests are started first and clippy
# a moment later, so the lock usually falls to the test build: the sequence is
# then build tests → (run tests ∥ clippy), and the lint's wall time disappears
# into the run's. The other order (clippy took the lock first) is the old
# serial gate, no slower than before. Clippy's output is held and printed once
# it ends, so a lint failure reads whole instead of interleaved with the test
# lines; the test output streams as it always did. Both verdicts are waited
# for, so a failing lint is reported even when the tests passed and the other
# way round. The gate's clippy argv follows `cargo clippy`.
# SUSI_GATE_SERIAL=1 keeps the old one-after-the-other gate (a machine whose
# timing-sensitive tests cannot spare the cores a concurrent clippy takes).
gate_lint_and_test() {
    local clippy_log test_status=0 lint_status=0 tpid
    if [ "${SUSI_GATE_SERIAL:-0}" = 1 ]; then
        timed_step clippy cargo clippy "$@" || return 1
        gate_test_steps
        return
    fi
    clippy_log=$(mktemp)
    gate_test_steps &
    tpid=$!
    sleep "${SUSI_GATE_LINT_DELAY:-2}"
    timed_step clippy cargo clippy "$@" >"$clippy_log" 2>&1 || lint_status=$?
    wait "$tpid" || test_status=$?
    cat "$clippy_log" >&2
    rm -f "$clippy_log"
    if [ "$lint_status" -ne 0 ]; then
        echo "❌ gate: clippy failed (exit $lint_status); see its output above" >&2
        return "$lint_status"
    fi
    return "$test_status"
}

# Whether the gate run above already covered a task's acceptance command. Only
# cargo test/nextest acceptances can be covered — a scripts/ checker asserts
# things no cargo suite does — and only when the gate's package selection
# contains the package the command tests (ALL covers everything).
acceptance_covered() {
    local cmd=$1 pkg rootpkg t
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
    rootpkg=$(sed -n 's/^name = "\(.*\)"/\1/p' Cargo.toml | head -1)
    [ -n "$pkg" ] || pkg=$rootpkg
    [ -n "$pkg" ] || return 1
    printf '%s\n' $gate_pkgs | grep -qx "$pkg" || return 1
    if [ "$pkg" = "$rootpkg" ]; then
        if [ -n "${gate_root_tests:-}" ]; then
            # The root gate narrowed to the touched `tests/<name>.rs` binaries; only
            # an acceptance naming one of those `--test <name>` targets was covered.
            t=$(printf '%s\n' "$cmd" | grep -oE -- '--test[ =][a-zA-Z0-9_-]+' | head -1 | grep -oE '[a-zA-Z0-9_-]+$')
            [ -n "$t" ] || return 1
            printf '%s\n' $gate_root_tests | grep -qx "$t" || return 1
        else
            case "$cmd" in *--test*) return 1 ;; esac
        fi
    fi
    return 0
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
    trap finish_cleanup EXIT
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
    export SUSI_SYNC_IN_FINISH=1
    # Without --wait the branch is published and the claim released seconds
    # after the close, so the close receipt rides that transaction instead of
    # a push of its own: one atomic push on a laptop (publish-task-branch.py
    # creates the receipt with the branch, the claim and the agent index), and
    # in a cloud session — where every queue write is an Actions run relaying
    # the push — one relayed run instead of two (`tasks release` publishes it,
    # SUSI_RELEASE_PUBLISH_RECEIPT). The claim is held until that transaction
    # lands, so nothing can take the task in between; a finish that dies after
    # the close leaves it closed under the claim, and the next finish publishes.
    # `--wait` keeps the close publishing its receipt first: its wait reads the
    # debt from the remote before the release.
    if [ "$wait" != 1 ]; then
        export SUSI_CLOSE_DEFER_RECEIPT="$task@$root"
        export SUSI_RELEASE_PUBLISH_RECEIPT="$task@$root"
        export SUSI_FINISH_REMOTE_GATE=1
    else
        unset SUSI_FINISH_REMOTE_GATE || true
    fi
    prefetch_finish_refs
    timed_step lease renew_or_readopt
    timed_step initial-sync sync
    timed_step gate gate
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
            SUSI_ACCEPTANCE_COVERED="$task@$root" timed_step close "$susi_bin" tasks close "$task"
        else
            timed_step close "$susi_bin" tasks close "$task"
        fi
        git add -- ".agents/tasks/$task.json" ".agents/tasks/done/$task.json"
        git commit -m "Close $task after acceptance" -m "Task: $task"
    fi
    [ -f ".agents/tasks/done/$task.json" ] || { echo 'No completed task record.' >&2; exit 1; }
    branch=$(git symbolic-ref --short HEAD)
    timed_step push push_closed_branch "$branch"
    sha=$(git rev-parse HEAD)
    if [ "$wait" != 1 ] || [ "${SUSI_FINISH_WAIT_MAX:-1800}" -le 0 ]; then
        # Never fall through into the wait below: a failed release used to
        # park the agent in a 30-minute poll it never asked for.
        if [ "${released_with_push:-0}" = 1 ]; then
            echo 'workflow: release included in the atomic branch push' >&2
        elif ! timed_step release "$susi_bin" tasks release "$task" --force; then
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
    # SUSI_SEED_REFRESH_SECS. Background compilation is opt-in: even at low
    # priority a full workspace build competes for memory, disk and linkers.
    # Existing snapshots still seed worktrees; agents can explicitly refresh
    # them during maintenance without delaying foreground iteration.
    seed_next=0
    seed_tick() {
        [ "${SUSI_SEED_AUTO_REFRESH:-}" = 1 ] || return 0
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
