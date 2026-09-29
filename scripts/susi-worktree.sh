#!/usr/bin/env bash
# Mandate 49: create your own worktree + branch off the latest origin/main.
#   scripts/susi-worktree.sh <name> [base]      (base defaults to origin/main)
set -euo pipefail
name=${1:?usage: scripts/susi-worktree.sh <name> [base]}
base=${2:-origin/main}
root=$(git rev-parse --show-toplevel)
git -C "$root" fetch -q origin
dest="$(dirname "$root")/$(basename "$root")-$name"
git -C "$root" worktree add -b "$name" "$dest" "$base"
echo "worktree: $dest (branch $name)"
# Keep the primary checkout parked at origin/main so it never drifts onto a
# stale branch that invites work (non-fatal: it refuses if it has real work).
"$root/scripts/park-primary.sh" || echo "note: primary checkout not parked (see above); it is still not for work" >&2
echo "next: cd $dest && scripts/setup-dev.sh && susi workflow check"
