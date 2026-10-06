#!/usr/bin/env bash
# Block until the full Test suite has passed on <sha>, the commit a release tag
# names; fail if it did not. The release builds four binaries for ~30 minutes and
# publishes them, so nothing builds until the commit they are built from is known
# to pass the suite on a clean machine.
#
#   release-await-tests.sh <owner/repo> <ref> <sha>
#
# 1. A passed full-suite run for <sha> is reused: a `workflow_dispatch` run of the
#    Test workflow (the post-merge suite auto-merge dispatches on main is one), or
#    a push run on main. A push run on any OTHER branch is not a full suite - it
#    compiles and lints - so it never counts.
# 2. Otherwise the Test workflow is dispatched on <ref> (the tag), which runs the
#    sharded suite, lint and e2e - not the coverage ratchet or the Kani proofs,
#    which gate merges to main and are skipped on a tag - and this waits for it.
#
# exit 0: the suite passed on <sha>
# exit 1: it failed, was cancelled, never started, or did not finish in time
set -uo pipefail
repo=${1:?repo} ref=${2:?ref} sha=${3:?sha}
wait_max=${RELEASE_TESTS_WAIT_MAX:-2700}
poll=${RELEASE_TESTS_POLL:-15}
find_max=${RELEASE_TESTS_FIND_MAX:-120}

# Runs of the Test workflow for $sha that executed the full suite.
full_suite_runs() {
    gh run list --repo "$repo" --workflow test.yml --commit "$sha" --limit 30 \
        --json databaseId,event,headBranch,status,conclusion 2>/dev/null |
        jq -c '.[] | select(.event == "workflow_dispatch" or (.event == "push" and .headBranch == "main"))'
}

reusable=$(full_suite_runs | jq -r 'select(.conclusion == "success") | .databaseId' | head -1)
if [ -n "$reusable" ]; then
    echo "the full suite already passed on $sha (run $reusable) — not run again"
    exit 0
fi

started=$(date +%s)
if ! gh workflow run test.yml --repo "$repo" --ref "$ref"; then
    echo "could not dispatch the Test workflow on $ref" >&2
    exit 1
fi
echo "dispatched the Test workflow on $ref ($sha); waiting for it"

# The dispatch returns no run id: find the run it created for this commit.
run=""
while [ -z "$run" ]; do
    run=$(gh run list --repo "$repo" --workflow test.yml --event workflow_dispatch \
        --limit 20 --json databaseId,headSha,createdAt 2>/dev/null |
        jq -r --arg sha "$sha" --argjson since "$((started - 10))" \
            '[.[] | select(.headSha == $sha and (.createdAt | fromdateiso8601) >= $since)] | sort_by(.createdAt) | last | .databaseId // empty')
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
        echo "the full suite passed on $sha (run $run)"
        exit 0
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
