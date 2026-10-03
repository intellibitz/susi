#!/usr/bin/env bash
# The three board checks in one compact block, for the loop boundary.
#
# The checkers were only as reliable as the habit of running them: they were
# hand-run during the audit and would otherwise stay dark. `sync` is the
# boundary every agent crosses before claiming, so they belong there — but two
# of the three cost ~5s each (they read the remote), which is too much to pay on
# every sync. So the verdict is cached briefly: the first sync in a window pays
# for it, everyone else reads it. Never blocking, because a stale board is not a
# reason to stop work.
#
#   scripts/swarm-status.sh            # cached block (at most every 5 min by default)
#   scripts/swarm-status.sh --force    # check now, refresh the cache
#
# Environment:
#   SUSI_BOARD_MAX_AGE   seconds a cached verdict stays valid (300; 0 = always)
#   SUSI_BOARD_TIMEOUT   seconds each checker may take (60)
set -uo pipefail

root=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
common=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || exit 0
cd "$root" || exit 0

max_age=${SUSI_BOARD_MAX_AGE:-300}
timeout_s=${SUSI_BOARD_TIMEOUT:-60}
stamp="$common/susi-board-status.stamp"
cache="$common/susi-board-status.txt"
cache_exit="$common/susi-board-status.exit"
[ "${1:-}" = "--force" ] && max_age=0

# Who is working on what, live (not cached: a claim is news, not a verdict).
# Without it an agent cannot tell whether the fix it is about to make is already
# being made — the one kind of duplication claims cannot prevent, because two
# agents only conflict once they reserve the same paths.
format_in_flight() {
    local out line
    out=$(jq -r '.open[] | select(.claimed_by) | "\(.id) by \(.claimed_by)"' 2>/dev/null || true)
    if [ -z "$out" ]; then
        echo "  in flight: nothing claimed right now"
        return 0
    fi
    line=$(printf '%s\n' "$out" | paste -sd, - | sed 's/,/, /g')
    echo "  in flight: ${line}"
}

in_flight() {
    command -v jq >/dev/null 2>&1 || return 0
    susi tasks list 2>/dev/null | format_in_flight
}

# The in-flight line is judged on its own, so its acceptance tests the script
# and not whether the board happened to be clean when it ran.
self_test() {
    local failures=0 got
    check() {
        got=$(printf '%s' "$2" | format_in_flight)
        if [ "$got" = "$3" ]; then
            echo "  self-test: $1 → ok"
        else
            echo "  self-test: $1 → WRONG ($got)"
            failures=$((failures + 1))
        fi
    }
    check "no claims" '{"open":[{"id":"T-A-1","claimed_by":null}]}' "  in flight: nothing claimed right now"
    check "one claim" '{"open":[{"id":"T-A-1","claimed_by":"AGENTA"},{"id":"T-B-2","claimed_by":null}]}' "  in flight: T-A-1 by AGENTA"
    check "several" '{"open":[{"id":"T-A-1","claimed_by":"A"},{"id":"T-B-1","claimed_by":"B"}]}' "  in flight: T-A-1 by A, T-B-1 by B"
    if [ "$failures" != 0 ]; then
        echo "self-test FAILED: $failures case(s) wrong"
        exit 1
    fi
    echo "PASS: the in-flight line names every holder, and says so when there are none"
    exit 0
}

[ "${1:-}" = "--self-test" ] && self_test

age=""
if [ -f "$stamp" ]; then
    age=$(( $(date +%s) - $(cat "$stamp" 2>/dev/null || echo 0) ))
fi

if [ -n "$age" ] && [ "$age" -lt "$max_age" ] && [ -f "$cache" ]; then
    cat "$cache"
    echo "  (board checked ${age}s ago; scripts/swarm-status.sh --force to re-check)"
    in_flight
    exit "$(cat "$cache_exit" 2>/dev/null || echo 0)"
fi

failed=0
out="swarm board:"
run_check() {
    local label=$1 script=$2 result code summary
    if [ ! -x "$script" ]; then
        out="$out
  ⏭️  ${label}: not in this checkout"
        return 0
    fi
    result=$(timeout "$timeout_s" "$script" 2>&1)
    code=$?
    summary=$(printf '%s\n' "$result" | grep -v '^$' | tail -1)
    case "$code" in
    0) ;; # passed: its own line already carries the ✅ and the checker's name
    124) summary="⚠️  ${label}: timed out after ${timeout_s}s" ;;
    *)
        failed=1
        # Show the first line that says what broke, not the trailing fix hint.
        summary=$(printf '%s\n' "$result" | grep -m1 '^❌' || printf '%s' "$summary")
        ;;
    esac
    out="$out
  ${summary}"
}

run_check "board hygiene" scripts/check-board-hygiene.py
run_check "roadmap queue" scripts/check-roadmap-queue.py
run_check "swarm readiness" scripts/check-swarm-readiness.py
# The one check that is about other agents rather than this checkout: who
# accepted work that never reached main. Visibility is the point — the debt
# already blocks its own agent's next claim.
run_check "owed merges" scripts/check-owed-merges.sh
if [ "$failed" != 0 ]; then
    out="$out
  (a ❌ here never blocks the loop — fix it under a claim, not instead of working)"
fi

# Written after the work, so a reader sees the old verdict or the new one.
printf '%s\n' "$out" >"$cache.tmp" && mv "$cache.tmp" "$cache"
date +%s >"$stamp"
echo "$failed" >"$cache_exit"
printf '%s\n' "$out"
in_flight
exit "$failed"
