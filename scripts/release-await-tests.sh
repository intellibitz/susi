#!/usr/bin/env bash
# Block until the full Test suite has passed on <sha>, the commit a release tag
# names; fail if it did not. The release builds four binaries for ~30 minutes and
# publishes them, so nothing builds until the commit they are built from is known
# to pass the suite on a clean machine.
#
#   release-await-tests.sh <owner/repo> <ref> <sha>
#
# 1. A passed full-suite run for <sha> is reused: a push run on main, or a
#    `workflow_dispatch` run of the Test workflow whose suite actually ran (the
#    post-merge suite auto-merge dispatches on main is one). A push run on any
#    OTHER branch is not a full suite - it compiles and lints - so it never
#    counts, and neither does the merge bot's `gate=branch` retest of a resynced
#    head: that is a dispatch too, but it runs exactly what a branch push runs.
# 2. Otherwise the Test workflow is dispatched on <ref> (the tag) with the full
#    gate, which runs the sharded suite, lint and e2e - not the coverage ratchet
#    or the Kani proofs, which gate merges to main and are skipped on a tag - and
#    this waits for it. A dispatch on main runs main's head as it is then, so
#    on main <sha> must still be that head (a commit main has moved past is
#    proven on a tag), and the dispatch is a `prove` run: test.yml gives one a
#    concurrency group of its own, where the merges' group would make it wait
#    behind the running suite past wait_max, or replace it while pending.
#
# exit 0: the suite passed on <sha>
# exit 1: it failed, was cancelled, never started, or did not finish in time
set -uo pipefail
[ ! -f "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh" ] || . "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh"
repo=${1:?repo} ref=${2:?ref} sha=${3:?sha}
wait_max=${RELEASE_TESTS_WAIT_MAX:-2700}
poll=${RELEASE_TESTS_POLL:-15}
find_max=${RELEASE_TESTS_FIND_MAX:-120}

# test.yml's `lint` job. Every full-gate dispatch that ran its suite ran it; a
# dispatched run on main the code-tree stamp proved by identity, or that
# reached no crate, skipped it with the shards (and is then not reused here:
# the suite is dispatched on the tag instead, which is what a release wants on
# a sha no run executed), and a `gate=branch` dispatch skips it with the
# shards and e2e. A push run on main is reused on its conclusion alone, as
# before: main takes no direct pushes (the ruleset allows pull requests only),
# so such a run is a hand push outside the loop and its reuse is unchanged. `gh run list` does not report a dispatch's inputs,
# so a green dispatch run whose job did not pass is taken for what it is: a
# branch gate that compiled and linted. tests/release_await_tests.rs pins the
# name against test.yml.
suite_job="Doc Tests + Clippy"

# Runs of the Test workflow for $sha that may have executed the full suite.
full_suite_runs() {
    gh run list --repo "$repo" --workflow test.yml --commit "$sha" --limit 30 \
        --json databaseId,event,headBranch,status,conclusion 2>/dev/null |
        jq -c '.[] | select(.event == "workflow_dispatch" or (.event == "push" and .headBranch == "main"))'
}

# Whether dispatched run $1 executed the suite and it passed: 0 it did, 1 it
# did not (a branch gate), 2 its jobs could not be read - which says nothing
# either way, so it is never taken for a branch gate.
ran_the_suite() {
    local verdict
    verdict=$(gh run view "$1" --repo "$repo" --json jobs \
        --jq "any(.jobs[]; .name == \"$suite_job\" and .conclusion == \"success\")" 2>/dev/null) || return 2
    [ "$verdict" = true ]
}

reusable=""
for entry in $(full_suite_runs | jq -r 'select(.conclusion == "success") | "\(.databaseId):\(.event)"'); do
    id=${entry%%:*}
    if [ "${entry#*:}" = push ]; then
        reusable=$id
        break
    fi
    ran_the_suite "$id"
    case $? in
    0)
        reusable=$id
        break
        ;;
    1) echo "run $id passed without running the suite (a branch-gate retest) — not reused" ;;
    esac
done
if [ -n "$reusable" ]; then
    echo "the full suite already passed on $sha (run $reusable) — not run again"
    exit 0
fi

# Dispatch the suite on $ref and note when, for the search that finds its run.
dispatched=0
dispatch_suite() {
    local -a dispatch=(gh workflow run test.yml --repo "$repo" --ref "$ref" -f gate=full)
    local tip
    case "$ref" in
    main | refs/heads/main)
        tip=$(gh api "repos/$repo/git/ref/heads/main" --jq .object.sha 2>/dev/null) || tip=""
        if [ "$tip" != "$sha" ]; then
            echo "main is at ${tip:-a commit the API did not name}, not $sha: a dispatch on main would test that tree — release a tag on $sha instead" >&2
            exit 1
        fi
        dispatch+=(-f prove=true)
        ;;
    esac
    started=$(date +%s)
    if ! "${dispatch[@]}"; then
        echo "could not dispatch the Test workflow on $ref" >&2
        exit 1
    fi
    dispatched=1
    echo "dispatched the Test workflow on $ref ($sha); waiting for it"
}

# A run for this commit that has not finished is watched before anything is
# dispatched: when a version bump merges, the merge bot dispatches the
# post-merge suite on main and then this workflow on the tag, so that suite is
# nearly always still running when this starts. Dispatching a second one ran
# the whole suite twice (v0.21.26: both runs started within 30 s, each took
# 19 min, and the four release legs waited for the tag's copy while main's
# finished first) and put two suites' worth of jobs in front of every branch
# push queued behind them. The in-flight run is judged exactly as a dispatched
# one is below: a green one that turns out to be a branch gate is passed over
# and the suite is dispatched then.
started=$(date +%s)
run=$(full_suite_runs | jq -r 'select(.status != "completed") | .databaseId' | head -n 1)
if [ -n "$run" ]; then
    echo "a run for $sha is still in flight (run $run) — watching it rather than dispatching the suite again"
else
    dispatch_suite
fi

# Runs this script watched to a green finish that turned out to be a branch-gate
# retest dispatched on the same commit in the same window, not its own dispatch.
passed_over=" "
while :; do
    # The dispatch returns no run id: find the run it created for this commit.
    while [ -z "$run" ]; do
        run=$(gh run list --repo "$repo" --workflow test.yml --event workflow_dispatch \
            --limit 20 --json databaseId,headSha,createdAt 2>/dev/null |
            jq -r --arg sha "$sha" --argjson since "$((started - 10))" --arg skip "$passed_over" \
                '[.[] | . as $r | select($r.headSha == $sha and ($r.createdAt | fromdateiso8601) >= $since
                    and ($skip | contains(" \($r.databaseId) ") | not))]
                 | sort_by(.createdAt) | last | .databaseId // empty')
        [ -n "$run" ] && break
        if [ $(($(date +%s) - started)) -ge "$find_max" ]; then
            echo "no Test run appeared for $sha within ${find_max}s of the dispatch" >&2
            exit 1
        fi
        sleep "$poll"
    done
    echo "watching run $run"

    while :; do
        state=$(gh run view "$run" --repo "$repo" --json status,conclusion \
            --jq '.status + "/" + (.conclusion // "")' 2>/dev/null || echo "unknown/")
        case "$state" in
        completed/success)
            ran_the_suite "$run"
            case $? in
            0)
                echo "the full suite passed on $sha (run $run)"
                exit 0
                ;;
            1)
                passed_over="$passed_over$run "
                if [ "$dispatched" = 1 ]; then
                    echo "run $run passed without running the suite (a branch-gate retest of $sha) — looking for the dispatched suite"
                else
                    # The in-flight run was a branch gate, not the suite: now
                    # there is nothing to wait for but a dispatch of our own.
                    echo "run $run passed without running the suite (a branch-gate retest of $sha) — dispatching the suite"
                    dispatch_suite
                fi
                run=""
                break
                ;;
            esac
            # Its jobs could not be read: ask again on the next poll.
            ;;
        completed/*)
            echo "the full suite did not pass on $sha: run $run ${state#completed/}" >&2
            gh run view "$run" --repo "$repo" --json jobs \
                --jq '.jobs[] | select(.conclusion == "failure") | "  failed job: " + .name' >&2 2>/dev/null || true
            exit 1
            ;;
        esac
        if [ $(($(date +%s) - started)) -ge "$wait_max" ]; then
            echo "run $run had not finished after ${wait_max}s ($state)" >&2
            exit 1
        fi
        sleep "$poll"
    done
done
