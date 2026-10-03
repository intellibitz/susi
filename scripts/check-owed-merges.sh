#!/usr/bin/env bash
# Did every agent publish what it accepted? `susi workflow check` answers only
# for *you*; this asks the swarm-wide question by reconciling the close
# receipts, abandonments and merge attestations on the shared remote against
# `origin/main` — the per-agent tally behind `susi tasks audit`.
#
#   scripts/check-owed-merges.sh     # 0 = every accepted task reached main
#
# Never blocks: the board block it feeds is advisory, and "could not read" is
# reported as unknown rather than as debt.
set -uo pipefail
root=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$root" || exit 0
susi_bin=${SUSI_WORKFLOW_BIN:-susi}
command -v "$susi_bin" >/dev/null 2>&1 || {
    echo "⏭️  owed merges: no $susi_bin on PATH"
    exit 0
}

out=$("$susi_bin" tasks audit --strict 2>&1)
code=$?

# A susi that predates the subcommand is "not available here", not a board
# failure: the release that ships it arrives on its own schedule, and agents
# run whatever is installed on their host.
if printf '%s' "$out" | grep -qE "unrecognized subcommand|unexpected argument|no such subcommand"; then
    echo "⏭️  owed merges: this $susi_bin has no 'tasks audit' yet"
    exit 0
fi
if [ "$code" = 0 ]; then
    echo "✅ owed merges: every accepted task is on origin/main"
    exit 0
fi

owed=$(printf '%s\n' "$out" | grep '^❌' || true)
if [ -z "$owed" ]; then
    echo "⏭️  owed merges: could not read the queue ($(printf '%s\n' "$out" | tail -1))"
    exit 0
fi
count=$(printf '%s\n' "$owed" | grep -c .)
printf '%s\n' "$owed" | head -5
[ "$count" -le 5 ] || echo "  … and $((count - 5)) more"
echo "❌ owed merges: $count accepted task(s) are not on origin/main — whoever accepted them owes the merge (susi workflow finish <id>, or: susi tasks release <id> --abandon <reason>)"
exit 1
