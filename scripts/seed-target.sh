#!/bin/sh
# Give a worktree its own cargo target, warm from a snapshot of origin/main.
#
#   scripts/seed-target.sh [DIR]               seed DIR's target/ (default: this
#                                              checkout) before anything builds
#   scripts/seed-target.sh --background [DIR]  replace a link now, copy in the
#                                              background (the git hooks)
#   scripts/seed-target.sh --refresh [--force] [--log FILE]
#                                              rebuild and republish the snapshot
#                                              (the primary watcher runs this)
#
# The work is susi's: `susi workflow seed-target` (crates/susi-gawd/src/admin/seed.rs)
# does everything this script did in bash, with git, cargo and the copy tool run
# directly, so an installed susi.exe seeds a worktree without Git's MSYS userland
# on PATH. This file stays so that the hooks, the docs and branches cut before the
# port, which run `scripts/seed-target.sh`, keep working: it finds a susi that has
# the verb — $SUSI_WORKFLOW_BIN (the susi driving a `workflow finish`), the
# installed susi on PATH, then this checkout's own build (target/release,
# target/debug; not through a target/ that is still a link to the shared one,
# which the seeding is about to remove) — and execs it, holding none of the
# caller's descriptors (a lock on fd 9, a pipe a hook reads to its end). A susi
# older than the verb is passed over, not run into. With none, it says so and
# exits 0: the seeding never fails its caller, and that first build is cold.
# Nothing here needs more than sh and git.
set -u
top=$(git rev-parse --show-toplevel 2>/dev/null) || top=
own=
if [ -n "$top" ] && [ ! -L "$top/target" ]; then
    own="$top/target"
fi
for bin in "${SUSI_WORKFLOW_BIN:-}" "$(command -v susi 2>/dev/null)" \
    "${own:+$own/release/susi}" "${own:+$own/release/susi.exe}" \
    "${own:+$own/debug/susi}" "${own:+$own/debug/susi.exe}"; do
    [ -n "$bin" ] && [ -x "$bin" ] || continue
    if "$bin" workflow seed-target --help >/dev/null 2>&1 </dev/null; then
        exec "$bin" workflow seed-target "$@" 3>&- 4>&- 5>&- 6>&- 7>&- 8>&- 9>&-
    fi
done
echo "target: no susi with \`workflow seed-target\` found (SUSI_WORKFLOW_BIN, the installed susi, this checkout's target/): nothing is seeded, and the first build compiles everything. Install the current release: scripts/susi-release-sync.sh" >&2
exit 0
