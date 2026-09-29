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
echo "next: cd $dest && scripts/setup-dev.sh"
