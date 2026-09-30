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

agent=${SUSI_AGENT:-$(git -C "$root" config user.name 2>/dev/null || echo agent)}
agent=$(printf '%s' "$agent" | tr '[:upper:]' '[:lower:]' | tr -c 'a-z0-9\n' '-' | sed 's/^-*//; s/-*$//')
name=${1:-"${agent:-agent}-$(date +%Y%m%d-%H%M%S)"}
base=${2:-origin/main}
case "$name" in
main | HEAD | "") echo "susi-worktree: refusing branch name '$name'" >&2; exit 2 ;;
esac

git -C "$root" fetch -q origin
dest="$(dirname "$root")/$(basename "$root")-$name"
if [ -d "$dest" ]; then
    echo "susi-worktree: $dest already exists — reusing it" >&2
else
    git -C "$root" worktree add -b "$name" "$dest" "$base" >&2
fi
echo "worktree: $dest (branch $name)" >&2

# Hooks + ledger merge driver for the clone (shared by every worktree of it).
(cd "$dest" && scripts/setup-dev.sh) >&2

# Keep the primary checkout parked at origin/main so it never drifts onto a
# stale branch that invites work (non-fatal: it refuses if it has real work).
"$root/scripts/park-primary.sh" >&2 || echo "note: primary checkout not parked (see above); it is still not for work" >&2

echo "ready: atomic loop — \`susi workflow check\`, then sync→claim one→work→commit→sync→push→sync" >&2
echo "cd $dest"
