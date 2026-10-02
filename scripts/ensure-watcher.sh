#!/usr/bin/env bash
# Is this clone's primary-checkout watcher alive, and restart it if not?
#
# The watcher fetches every 15s and fast-forwards a clean primary `main`, so the
# primary never drifts into a tree that invites work (Mandate 49). It is a
# background process: it dies with whatever session or terminal started it, and
# nothing used to restart it — the audit found it five hours stale while the
# primary quietly stopped converging, and only `susi workflow check` said so.
# So the loop heals it at the boundary every agent crosses (`sync`).
#
# Exit 0 when a watcher is alive or was started; 1 when one is stale and
# `--dry-run` asked only for the decision.
#
#   scripts/ensure-watcher.sh              # restart if stale (never blocks)
#   scripts/ensure-watcher.sh --dry-run    # report the decision, change nothing
#
# Environment:
#   SUSI_WATCH_STALE   seconds after which a heartbeat counts as stale (120)
#   SUSI_WATCH_CMD     what to start (default: this repo's watcher)
#   SUSI_WATCH_WAIT    seconds to wait for a fresh heartbeat after starting (10)
set -uo pipefail

root=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
common=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || exit 0
stamp="$common/susi-primary-watch.stamp"
log="$common/susi-primary-watch.log"
stale_after=${SUSI_WATCH_STALE:-120}
wait_after=${SUSI_WATCH_WAIT:-10}
dry_run=0
[ "${1:-}" = "--dry-run" ] && dry_run=1

heartbeat() {
    local value
    value=$(cat "$stamp" 2>/dev/null) || { echo ""; return; }
    case "$value" in '' | *[!0-9]*) echo "" ;; *) echo "$value" ;; esac
}

age() {
    local beat now
    beat=$(heartbeat)
    [ -n "$beat" ] || { echo ""; return; }
    now=$(date +%s)
    echo $((now - beat))
}

current_age=$(age)
if [ -n "$current_age" ] && [ "$current_age" -le "$stale_after" ]; then
    echo "primary watcher alive (heartbeat ${current_age}s ago)"
    exit 0
fi

state="never started"
[ -n "$current_age" ] && state="heartbeat ${current_age}s ago"
if [ "$dry_run" = 1 ]; then
    echo "primary watcher stale ($state) — would restart"
    exit 1
fi

# The watcher takes the lock itself and exits when another one holds it, so
# starting unconditionally is safe: a live-but-slow watcher is not duplicated.
cmd=${SUSI_WATCH_CMD:-"$root/scripts/parallel-workflow.sh watch"}
# shellcheck disable=SC2086 # deliberately word-split: the command is argv words
nohup $cmd >"$log" 2>&1 </dev/null &
started=$!

deadline=$(( $(date +%s) + wait_after ))
while [ "$(date +%s)" -le "$deadline" ]; do
    fresh=$(age)
    if [ -n "$fresh" ] && [ "$fresh" -le "$stale_after" ]; then
        echo "primary watcher was stale ($state) — restarted (pid $started)"
        exit 0
    fi
    sleep 1
done

# Non-fatal by design: a dead watcher degrades convergence, it does not stop
# work, and `susi workflow check` reports it too.
echo "⚠️  primary watcher was stale ($state) and did not heartbeat within ${wait_after}s — start it: susi workflow watch &" >&2
exit 0
