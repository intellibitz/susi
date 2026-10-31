#!/bin/sh
# Mandate 49: create your own worktree + branch off the latest origin/main,
# ready for the atomic per-task loop (sync→claim→work→commit→sync→push→sync).
#
#   scripts/susi-worktree.sh              auto name: <tool>-<yyyymmdd>-<hhmmss>-<pid>
#   scripts/susi-worktree.sh <name> [base]   (base defaults to origin/main)
#
# It fetches, creates the worktree, installs the hooks and merge driver in it,
# parks the primary checkout at origin/main, starts the primary watcher, seeds
# the worktree's own cargo target in the background, and prints `cd <path>` as
# the LAST line so a caller can `eval "$(scripts/susi-worktree.sh | tail -1)"`.
# In a cloud session's clone it installs the hooks and prints `cd` back into it.
#
# The work is susi's: `susi workflow start` (crates/susi-gawd/src/admin/
# workflow.rs) does everything this script did in bash, with git run directly
# and the agent tool read from this process's ancestors without `ps -o`, so an
# installed susi.exe makes a worktree without Git's MSYS userland (flock, awk,
# sed, tr, cygpath) on PATH. This file stays so that the docs and branches cut
# before the port, which run `scripts/susi-worktree.sh`, keep working: it finds a
# susi that has the verb — $SUSI_WORKFLOW_BIN, the installed susi on PATH, then
# this checkout's own build and the build of the checkout this file is in
# (target/release, target/debug; not through a target/ that is a link) — and
# execs it. With none it makes nothing, says so and fails: a worktree without
# its hooks and identity is a trap, not a start. Nothing here needs more than sh
# and git.
set -u
top=$(git rev-parse --show-toplevel 2>/dev/null) || top=
own=
if [ -n "$top" ] && [ ! -L "$top/target" ]; then
    own="$top/target"
fi
mine=$(cd "$(dirname "$0")/.." 2>/dev/null && pwd) || mine=
here=
if [ -n "$mine" ] && [ ! -L "$mine/target" ]; then
    here="$mine/target"
fi
for bin in "${SUSI_WORKFLOW_BIN:-}" "$(command -v susi 2>/dev/null)" \
    "${own:+$own/release/susi}" "${own:+$own/release/susi.exe}" \
    "${own:+$own/debug/susi}" "${own:+$own/debug/susi.exe}" \
    "${here:+$here/release/susi}" "${here:+$here/release/susi.exe}" \
    "${here:+$here/debug/susi}" "${here:+$here/debug/susi.exe}"; do
    [ -n "$bin" ] && [ -x "$bin" ] || continue
    if "$bin" workflow start --help >/dev/null 2>&1 </dev/null; then
        exec "$bin" workflow start "$@" 3>&- 4>&- 5>&- 6>&- 7>&- 8>&- 9>&-
    fi
done
echo "susi-worktree: no susi with \`workflow start\` found (SUSI_WORKFLOW_BIN, the installed susi, this checkout's target/): no worktree made. Install the current release: scripts/susi-release-sync.sh — or build this checkout: cargo build" >&2
exit 1
