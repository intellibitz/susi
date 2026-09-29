#!/usr/bin/env bash
# Keep the primary checkout parked at origin/main so nobody works in it
# (Mandate 49): on `main`, fast-forwarded to origin/main. Only if another
# worktree already holds `main` (git allows a branch in one worktree) does it
# fall back to a detached HEAD at origin/main. workflow-guard and
# `susi workflow check` already refuse commits here; this removes the stale
# branch that invited them, and it never undoes a human's own `git switch main`.
#
#   scripts/park-primary.sh            park (run from any worktree of the repo)
#
# Refuses — and says why — when the primary checkout has uncommitted changes,
# an operation in progress, or commits that are not in origin/main yet.
set -euo pipefail
common=$(git rev-parse --path-format=absolute --git-common-dir)
primary=$(dirname "$common")
cd "$primary"
[ "$(git rev-parse --path-format=absolute --git-dir)" = "$common" ] \
    || { echo "park-primary: $primary is not the primary checkout" >&2; exit 2; }

git fetch --quiet origin 2>/dev/null || { echo "park-primary: cannot fetch origin; not moving anything" >&2; exit 1; }
target=$(git rev-parse --verify origin/main)

if [ -n "$(git status --porcelain)" ]; then
    echo "park-primary: refusing — $primary has uncommitted changes:" >&2
    git status --short | head -10 >&2
    exit 1
fi
for op in MERGE_HEAD CHERRY_PICK_HEAD REVERT_HEAD rebase-merge rebase-apply; do
    if [ -e "$common/$op" ]; then
        echo "park-primary: refusing — an operation is in progress in $primary ($op)" >&2
        exit 1
    fi
done
unmerged=$(git rev-list --count "$target..HEAD")
if [ "$unmerged" != 0 ]; then
    branch=$(git symbolic-ref -q --short HEAD || echo "detached HEAD")
    echo "park-primary: refusing — $unmerged commit(s) on $branch are not in origin/main." >&2
    echo "  push and merge them (or move them to a worktree branch) first." >&2
    exit 1
fi
# Does another worktree hold `main`?
main_elsewhere=$(git worktree list --porcelain | awk -v p="$primary" '
    /^worktree /{wt=substr($0,10)} /^branch refs\/heads\/main$/{ if (wt!=p) print wt }' | head -1)
branch=$(git symbolic-ref -q --short HEAD || true)

if [ -z "$main_elsewhere" ]; then
    # `main` is free: park ON it. Local main commits would be lost work.
    if git rev-parse --verify -q main >/dev/null && [ "$(git rev-list --count "$target..main")" != 0 ]; then
        echo "park-primary: refusing — local main has commits that are not in origin/main." >&2
        exit 1
    fi
    if [ "$branch" = main ] && [ "$(git rev-parse HEAD)" = "$target" ]; then
        echo "park-primary: already on main at origin/main (${target:0:8})"
        exit 0
    fi
    was=${branch:-$(git rev-parse --short HEAD)}
    git switch --quiet main 2>/dev/null || git switch --quiet -c main --track origin/main
    git merge --quiet --ff-only "$target"
    echo "park-primary: $primary moved from $was to main at origin/main (${target:0:8})"
    exit 0
fi

# Another worktree holds main: detach at origin/main instead.
if [ -z "$branch" ] && [ "$(git rev-parse HEAD)" = "$target" ]; then
    echo "park-primary: already parked at origin/main (${target:0:8}), detached ($main_elsewhere holds main)"
    exit 0
fi
was=${branch:-$(git rev-parse --short HEAD)}
git checkout --quiet --detach "$target"
echo "park-primary: $primary moved from $was to origin/main (${target:0:8}), detached ($main_elsewhere holds main)"
