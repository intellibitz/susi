#!/bin/sh
# Keep the primary checkout parked at origin/main so nobody works in it
# (Mandate 49): on `main`, fast-forwarded to origin/main; detached there only
# when another worktree holds `main`. It never undoes a human's own `git switch
# main`, and refuses, saying why, when the primary checkout has uncommitted
# changes, an operation in progress, or commits that are not in origin/main.
#
#   scripts/park-primary.sh              park (run from any worktree of the repo)
#   scripts/park-primary.sh --sync-only  keep an already-parked main current: fast-
#                                        forward it to origin/main, and do nothing
#                                        (silently) unless the primary checkout is on
#                                        main, clean and has no local commits. Safe
#                                        to run from every workflow check and hook.
#
# The work is susi's: `susi workflow park-primary` (crates/susi-gawd/src/admin/
# park.rs) does everything this script did in bash, with git run directly, so an
# installed susi.exe keeps the primary current without Git's MSYS userland on
# PATH. This file stays so that the hooks (.githooks/sync-primary), the loop
# scripts, the session-start hook and branches cut before the port, which run
# `scripts/park-primary.sh`, keep working: it finds a susi that has the verb —
# $SUSI_WORKFLOW_BIN (the susi driving a `workflow finish`), the installed susi on
# PATH, then this checkout's own build and the build of the checkout this file is
# in (target/release, target/debug; not through a target/ that is a link) — and
# execs it. A susi older than the verb is passed over, not run into. With none,
# --sync-only is silent (it is best effort everywhere it runs) and a park says so
# and fails, as a park that could not move anything did. Nothing here needs more
# than sh and git.
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
    if "$bin" workflow park-primary --help >/dev/null 2>&1 </dev/null; then
        exec "$bin" workflow park-primary "$@" 3>&- 4>&- 5>&- 6>&- 7>&- 8>&- 9>&-
    fi
done
case " $* " in
*" --sync-only "*) exit 0 ;;
esac
echo "park-primary: no susi with \`workflow park-primary\` found (SUSI_WORKFLOW_BIN, the installed susi, this checkout's target/): not moving anything. Install the current release: scripts/susi-release-sync.sh" >&2
exit 1
