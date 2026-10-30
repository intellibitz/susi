#!/usr/bin/env bash
# Block release publication until the release-required Test jobs have passed on
# <sha>, the commit a release tag names. The build legs run in parallel with
# this check, so they do not wait idle for the suite.
#
#   release-await-tests.sh <owner/repo> <ref> <sha>
#
# 1. A run on <sha> is reused once its release-required jobs have passed. The
#    run may still be finishing merge-only coverage or Kani work. A branch push
#    is not a full suite - it compiles and lints - so it never counts, and
#    neither does the merge bot's `gate=branch` retest of a resynced head: that
#    is a dispatch too, but it runs exactly what a branch push runs.
# 2. Otherwise the Test workflow is dispatched on <ref> (the tag) with the full
#    gate, which runs the sharded suite, lint and e2e - not the coverage ratchet
#    or the Kani proofs, which gate merges to main and are skipped on a tag - and
#    this waits for it. A dispatch on main runs main's head as it is then, so
#    on main <sha> must still be that head (a commit main has moved past is
#    proven on a tag), and the dispatch is a `prove` run: test.yml gives one a
#    concurrency group of its own, where the merges' group would make it wait
#    behind the running suite past wait_max, or replace it while pending.
#
# exit 0: release-required tests and repository checks passed on <sha>
# exit 1: a required job failed, the suite never started, or it timed out
set -uo pipefail
[ ! -f "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh" ] || . "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh"
repo=${1:?repo} ref=${2:?ref} sha=${3:?sha}
wait_max=${RELEASE_TESTS_WAIT_MAX:-2700}
poll=${RELEASE_TESTS_POLL:-15}
find_max=${RELEASE_TESTS_FIND_MAX:-120}

# `Doc Tests + Clippy` runs with the shards, and a full-gate dispatch whose
# code-tree stamp skipped the suite or a `gate=branch` retest skips both. Those
# runs cannot prove the release. `gh run list` does not report dispatch inputs,
# so the jobs themselves are the source of truth. tests/release_await_tests.rs
# pins their names and conditions against test.yml.
suite_job="Doc Tests + Clippy"

# Release proof is the test shards, doc-tests/clippy, and any live e2e job that
# actually ran, plus the cheap repository checks. Coverage and Kani are merge
# gates, not release gates; the tag run skips both. Waiting for the whole main
# workflow kept release waiting for 11 minutes on the 2026-10-30 main run,
# even though every release-required job was green by minute 6. Test
# job names and their conditions are pinned by tests/release_await_tests.rs.
release_jobs_state() {
    local snapshot summary
    run_status=unknown
    run_conclusion=unknown
    snapshot=$(gh run view "$1" --repo "$repo" --json status,conclusion,jobs \
        --jq '
          . as $run | ($run.jobs // []) as $jobs |
          ($jobs | map(select(.name == "Doc Tests + Clippy")) | first // null) as $lint |
          ($jobs | map(select(.name | startswith("Test (")))) as $shards |
          ($jobs | map(select(.name == "capability_gap_e2e (live susi-native)")) | first // null) as $e2e |
          ["Unsafe Exposure Ratchet", "Format Check", "Repository Invariants", "cargo deny (licenses + advisories)"] as $check_names |
          [ $check_names[] as $name | ($jobs | map(select(.name == $name)) | first // null) ] as $checks |
          [ $run.status, ($run.conclusion // "none"),
          (if ($lint != null and $lint.conclusion == "success"
              and ($shards | length) > 0
              and all($shards[]; .conclusion == "success")
              and all($checks[]; .conclusion == "success")
              and ($e2e == null or $e2e.conclusion == "success" or $e2e.conclusion == "skipped")) then "ready"
          elif (($lint != null and $lint.conclusion == "failure")
                or any($shards[]; .conclusion == "failure")
                or any($checks[]; .conclusion == "failure")
                or ($e2e != null and $e2e.conclusion == "failure")) then "failed"
          elif (($lint == null or $lint.conclusion == "skipped")
                and (($shards | length) == 0 or all($shards[]; .conclusion == "skipped"))) then "no-suite"
          else "pending" end) ] | @tsv
        ' 2>/dev/null) || return 4
    IFS=$'\t' read -r run_status run_conclusion summary <<<"$snapshot"
    case "$summary" in
    ready) return 0 ;;
    failed) return 1 ;;
    no-suite) return 2 ;;
    *) return 3 ;;
    esac
}

# Runs of the Test workflow for $sha that may have executed release-required jobs.
full_suite_runs() {
    gh run list --repo "$repo" --workflow test.yml --commit "$sha" --limit 30 \
        --json databaseId,event,headBranch,status,conclusion 2>/dev/null |
        jq -c '.[] | select(.event == "workflow_dispatch" or (.event == "push" and .headBranch == "main"))'
}

reusable=""
for entry in $(full_suite_runs | jq -r 'select(.status == "completed") | "\(.databaseId):\(.event)"'); do
    id=${entry%%:*}
    release_jobs_state "$id"
    case $? in
    0)
        reusable=$id
        break
        ;;
    1) echo "run $id did not pass the release-required jobs — not reused" ;;
    2) echo "run $id finished without running the suite (a branch-gate or code-tree skip) — not reused" ;;
    esac
done
if [ -n "$reusable" ]; then
    echo "the release-required checks already passed on $sha (run $reusable) — not run again"
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
        release_jobs_state "$run"
        verdict=$?
        state=${run_status:-unknown}
        case "$verdict" in
        0)
            echo "the release-required checks passed on $sha (run $run)"
            exit 0
            ;;
        1)
            echo "the release-required checks did not pass on $sha (run $run, $state)" >&2
            gh run view "$run" --repo "$repo" --json jobs \
                --jq '.jobs[] | select(.conclusion == "failure") | "  failed job: " + .name' >&2 2>/dev/null || true
            exit 1
            ;;
        2)
            case "$run_status/$run_conclusion" in
            completed/success)
                passed_over="$passed_over$run "
                if [ "$dispatched" = 1 ]; then
                    echo "run $run passed without running the suite (a branch-gate retest of $sha) — looking for the dispatched suite"
                else
                    echo "run $run passed without running the suite (a branch-gate retest of $sha) — dispatching the suite"
                    dispatch_suite
                fi
                run=""
                break
                ;;
            completed/*)
                echo "run $run finished without a release suite; dispatching on $ref" >&2
                if [ "$dispatched" = 1 ]; then exit 1; fi
                dispatch_suite
                run=""
                break
                ;;
            esac
            ;;
        esac
        if [[ "$run_status" == completed && "$verdict" != 4 ]]; then
            echo "the release-required checks did not pass on $sha: run $run ${run_conclusion:-unknown}" >&2
            gh run view "$run" --repo "$repo" --json jobs \
                --jq '.jobs[] | select(.conclusion == "failure") | "  failed job: " + .name' >&2 2>/dev/null || true
            exit 1
        fi
        if [ $(($(date +%s) - started)) -ge "$wait_max" ]; then
            echo "run $run had not finished after ${wait_max}s ($state)" >&2
            exit 1
        fi
        sleep "$poll"
    done
done
