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
# older than the verb is passed over, not run into. With none (the installed
# release predates the verb and the worktree has no build of its own yet), it
# still does the one thing that cannot wait for a release: a worktree's
# target/ that is a link to the clone's old shared target is unlinked (the
# link only), and a missing one is made, empty, so neither the cargo the
# caller runs next nor v0.21.23's `workflow start` (which links a target/ that
# does not exist) builds where a sibling's build can stand in for this
# branch's. Nothing is copied: that first build is cold. It says so and exits
# 0: the seeding never fails its caller. Nothing here needs more than sh and git.
set -u
top=$(git rev-parse --show-toplevel 2>/dev/null) || top=
own=
if [ -n "$top" ] && [ ! -L "$top/target" ]; then
    own="$top/target"
fi
# Primary checkout of this clone (first worktree listed): a brand-new worktree
# has no build of its own yet, and the installed release often predates the
# verb, so without this probe every `workflow start` paid a cold compile while
# the primary already held a seed-capable susi.
primary=
if [ -n "$top" ]; then
    primary=$(git -C "$top" worktree list --porcelain 2>/dev/null | awk '/^worktree /{print $2; exit}')
fi
primary_target=
if [ -n "$primary" ] && [ -d "$primary/target" ] && [ ! -L "$primary/target" ]; then
    primary_target="$primary/target"
fi
# Sibling worktrees (susi-*) often hold a newer debug build than the installed
# release; probing them keeps a fresh worktree from compiling cold while one of
# them already can seed.
sibling_bins=
if [ -n "$top" ]; then
    parent=$(dirname "$top")
    for cand in "$parent"/susi*/target/release/susi "$parent"/susi*/target/release/susi.exe \
        "$parent"/susi*/target/debug/susi "$parent"/susi*/target/debug/susi.exe; do
        [ -x "$cand" ] || continue
        sibling_bins="$sibling_bins $cand"
    done
fi
# shellcheck disable=SC2086
for bin in "${SUSI_WORKFLOW_BIN:-}" "$(command -v susi 2>/dev/null)" \
    "${own:+$own/release/susi}" "${own:+$own/release/susi.exe}" \
    "${own:+$own/debug/susi}" "${own:+$own/debug/susi.exe}" \
    "${primary_target:+$primary_target/release/susi}" "${primary_target:+$primary_target/release/susi.exe}" \
    "${primary_target:+$primary_target/debug/susi}" "${primary_target:+$primary_target/debug/susi.exe}" \
    $sibling_bins; do
    [ -n "$bin" ] && [ -x "$bin" ] || continue
    if "$bin" workflow seed-target --help >/dev/null 2>&1 </dev/null; then
        exec "$bin" workflow seed-target "$@" 3>&- 4>&- 5>&- 6>&- 7>&- 8>&- 9>&-
    fi
done
echo "target: no susi with \`workflow seed-target\` found (SUSI_WORKFLOW_BIN, installed, this/primary/sibling target/): nothing is copied, and the first build compiles everything. Install the current release: scripts/susi-release-sync.sh" >&2
# The worktree named (the last argument that is no flag), else this one;
# --refresh has nothing to fall back on.
dir=
for arg in "$@"; do
    case "$arg" in
    --refresh) exit 0 ;;
    --log) dir=-- ;;
    -*) ;;
    *) [ "$dir" = -- ] && dir= || dir=$arg ;;
    esac
done
[ "$dir" != -- ] || dir=
[ -n "$dir" ] || dir=$top
[ -n "$dir" ] && [ -d "$dir" ] || exit 0
top=$(git -C "$dir" rev-parse --show-toplevel 2>/dev/null) || exit 0
# Never the primary checkout: its target is the one worktrees still link.
gitdir=$(git -C "$top" rev-parse --path-format=absolute --git-dir 2>/dev/null) || exit 0
common=$(git -C "$top" rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || exit 0
[ "$gitdir" != "$common" ] || exit 0
t=$top/target
if [ -L "$t" ]; then
    rm -f -- "$t" 2>/dev/null
    if [ -L "$t" ]; then
        # Git for Windows' rm may refuse a junction; cmd's rmdir without /s
        # removes the junction only. The path is its own argument.
        case "$(uname -s 2>/dev/null)" in
        MSYS* | MINGW* | CYGWIN*) MSYS2_ARG_CONV_EXCL='*' cmd.exe /d /c rmdir "$(cygpath -aw "$t" 2>/dev/null || printf '%s' "$t")" >/dev/null 2>&1 ;;
        esac
    fi
    if [ -L "$t" ]; then
        echo "target: could not remove the link at $t; it still points at a shared target" >&2
        exit 0
    fi
    echo "target: replaced the link to a shared target with this worktree's own ($t)" >&2
fi
[ -e "$t" ] || mkdir "$t" 2>/dev/null
exit 0
