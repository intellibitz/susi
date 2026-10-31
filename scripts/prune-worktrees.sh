#!/bin/sh
# Reclaim finished agent worktrees — only the ones that cannot lose work: not
# the primary checkout, not the one this runs from, clean, with a HEAD already
# contained in origin/main, and (without --all) this agent's own. Anything with
# unmerged commits or uncommitted changes is reported and kept; branches are
# left alone; it fetches first and refuses to act at all when origin is
# unreachable.
#
#   scripts/prune-worktrees.sh                 # report only (the default)
#   scripts/prune-worktrees.sh --apply         # remove YOUR finished worktrees
#   scripts/prune-worktrees.sh --apply --all   # remove any provably finished one
#
# The work is susi's: `susi workflow prune` (crates/susi-gawd/src/admin/
# workflow.rs) decides and removes, with git run directly, so an installed
# susi.exe reclaims worktrees without Git's MSYS userland (awk, jq, cygpath) on
# PATH. This file stays so that the docs and branches cut before the port, which
# run `scripts/prune-worktrees.sh`, keep working: it finds a susi that has the
# verb — $SUSI_WORKFLOW_BIN, the installed susi on PATH, then this checkout's own
# build and the build of the checkout this file is in (target/release,
# target/debug; not through a target/ that is a link) — and execs it. With none
# it removes nothing, says so and fails: a prune that cannot judge must not
# guess. Nothing here needs more than sh and git.
set -u
for arg in "$@"; do
    case "$arg" in
    --apply | --all) ;;
    *)
        echo "usage: prune-worktrees.sh [--apply] [--all]" >&2
        exit 2
        ;;
    esac
done
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
    if "$bin" workflow prune --help >/dev/null 2>&1 </dev/null; then
        exec "$bin" workflow prune "$@" 3>&- 4>&- 5>&- 6>&- 7>&- 8>&- 9>&-
    fi
done
echo "❌ no susi with \`workflow prune\` found (SUSI_WORKFLOW_BIN, the installed susi, this checkout's target/): nothing reclaimed. Install the current release: scripts/susi-release-sync.sh" >&2
exit 1
