#!/usr/bin/env bash
# Mandate 49: create your own worktree + branch off the latest origin/main,
# ready for the atomic per-task loop (sync→claim→work→commit→sync→push→sync).
#
#   scripts/susi-worktree.sh              auto name: <agent>-<timestamp>
#   scripts/susi-worktree.sh <name> [base]   (base defaults to origin/main)
#
# It fetches, creates the worktree, installs the hooks and merge driver in it,
# parks the primary checkout at origin/main, and prints `cd <path>` as the LAST
# line so a caller can `eval "$(scripts/susi-worktree.sh | tail -1)"`.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
common=$(git rev-parse --path-format=absolute --git-common-dir)
# Startup changes shared refs/config; serialize it within this clone.
exec 9>"$common/susi-worktree-start.lock"
flock 9
primary=$(dirname "$common")

agent=${SUSI_AGENT:-$(git -C "$root" config user.name 2>/dev/null || echo agent)}
agent=$(printf '%s' "$agent" | tr '[:upper:]' '[:lower:]' | tr -c 'a-z0-9\n' '-' | sed 's/^-*//; s/-*$//')
name=${1:-"${agent:-agent}-$(date +%Y%m%d-%H%M%S)-${BASHPID:-$$}"}
base=${2:-origin/main}
case "$name" in
main | HEAD | "") echo "susi-worktree: refusing branch name '$name'" >&2; exit 2 ;;
esac

git -C "$root" fetch -q origin
dest="$(dirname "$primary")/$(basename "$primary")-$name"
if [ -d "$dest" ]; then
    [ "$(git -C "$dest" rev-parse --path-format=absolute --git-common-dir)" = "$common" ] \
        && [ "$(git -C "$dest" symbolic-ref -q --short HEAD)" = "$name" ] \
        || { echo "susi-worktree: refusing to reuse an unrelated checkout" >&2; exit 1; }
    echo "susi-worktree: $dest already exists — reusing it" >&2
else
    git -C "$root" worktree add -b "$name" "$dest" "$base" >&2
fi
echo "worktree: $dest (branch $name)" >&2

# Persist a unique worker identity, even when all tools share one Git login.
git -C "$root" config extensions.worktreeConfig true
worker=$(printf '%s' "$name" | tr '[:lower:]' '[:upper:]' | tr -cd 'A-Z0-9')
if ! git -C "$dest" config --worktree --get susi.agent >/dev/null; then
    git -C "$dest" config --worktree susi.agent "$worker"
fi

# Hooks + ledger merge driver for the clone (shared by every worktree of it).
(cd "$dest" && scripts/setup-dev.sh) >&2

# Keep the primary checkout parked at origin/main so it never drifts onto a
# stale branch that invites work (non-fatal: it refuses if it has real work).
"$root/scripts/park-primary.sh" >&2 || echo "note: primary checkout not parked (see above); it is still not for work" >&2

# A remote merge cannot invoke local hooks: keep a local singleton watcher.
if [ -x "$dest/scripts/parallel-workflow.sh" ] && [ -z "${SUSI_PRIMARY_WATCH_DISABLE:-}" ]; then
    (cd "$dest" && scripts/parallel-workflow.sh start-watch) 9>&-
fi

echo "ready: atomic loop — \`susi workflow check\`, then sync→claim one→work→commit→sync→push→sync" >&2
echo "cd $dest"
