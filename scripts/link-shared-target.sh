#!/usr/bin/env bash
# Point a linked worktree's `target/` at the primary checkout's, so one clone
# shares ONE cargo build cache instead of one per worktree.
#
#   scripts/link-shared-target.sh [dir]     (default: the current worktree)
#
# scripts/susi-worktree.sh does this for the worktrees it creates. Most agent
# sessions here are created by the desktop app instead, and a worktree it makes
# keeps a private `target/`: a fresh one compiles all ~716 crates (about 11
# minutes) where one sharing the primary's compiles only the ~80 workspace crates
# (about 80 seconds) - provided .cargo/config.toml embeds no worktree path, which
# tests/ci_workflow_perf.rs pins. The SessionStart hook
# (scripts/workflow-session-start.sh) runs this, so those sessions get it too.
#
# Idempotent, quiet when there is nothing to do, and it never fails a caller. It
# leaves alone:
#   - the primary checkout (nothing to share it with),
#   - SUSI_PRIVATE_TARGET=1 (the opt-out),
#   - a link that is already there,
#   - a `target/` directory that holds anything: that is somebody's build, and
#     replacing it would throw away a warm cache. An empty one is a leftover and
#     is replaced.
#
# Cost, stated: cargo locks the build directory, so two sessions compiling at the
# same moment wait for each other. With most units already built the compile
# phases are short, and SUSI_PRIVATE_TARGET=1 trades the wait for the cold build.
set -uo pipefail

dir=${1:-$(git rev-parse --show-toplevel 2>/dev/null)}
[ -n "$dir" ] && [ -d "$dir" ] || exit 0
[ -z "${SUSI_PRIVATE_TARGET:-}" ] || exit 0

common=$(git -C "$dir" rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || exit 0
primary=$(dirname "$common")
[ "$(cd "$dir" && pwd -P)" != "$(cd "$primary" 2>/dev/null && pwd -P)" ] || exit 0

link=$dir/target
[ -L "$link" ] && exit 0
if [ -e "$link" ]; then
    # rmdir only removes an empty directory; anything else stays untouched.
    rmdir "$link" 2>/dev/null || exit 0
fi

shared=$primary/target
mkdir -p "$shared" 2>/dev/null || exit 0
ln -sfn "$shared" "$link" 2>/dev/null || exit 0
echo "target: shared -> $shared" >&2
exit 0
