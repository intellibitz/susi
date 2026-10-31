#!/usr/bin/env bash
# Reclaim finished agent worktrees — only the ones that cannot lose work.
#
# A clone accumulates one worktree per task and nothing ever removed them: this
# repository reached 14, twelve of them behind `origin/main` by 50-262 commits,
# two holding commits that were never merged. A stale worktree is not just disk:
# it is a directory an agent can be resurrected into, with a branch and a token
# that no longer mean anything.
#
# Safe by construction. A worktree is reclaimed only when
#   - it is not the primary checkout and not the one this command runs from,
#   - its working tree is clean (`git worktree remove` refuses a dirty one too,
#     so this is defence in depth), and
#   - its HEAD is already contained in `origin/main` — nothing can be lost.
# Anything with unmerged commits or uncommitted changes is reported and kept.
# Branches are left alone; only the checkout is reclaimed. It fetches first, so
# "merged" means merged now, and it refuses to act at all when origin is
# unreachable.
#
# Another agent's worktree is never removed by default: `--apply` reclaims only
# worktrees whose `susi.agent` is this one. `--all` is the deliberate override
# for a human cleaning up a shared clone.
#
#   scripts/prune-worktrees.sh                 # report only (the default)
#   scripts/prune-worktrees.sh --apply         # remove YOUR finished worktrees
#   scripts/prune-worktrees.sh --apply --all   # remove any provably finished one
set -uo pipefail

apply=0
everything=0
for arg in "$@"; do
    case "$arg" in
    --apply) apply=1 ;;
    --all) everything=1 ;;
    *)
        echo "usage: prune-worktrees.sh [--apply] [--all]" >&2
        exit 2
        ;;
    esac
done

common=$(git rev-parse --path-format=absolute --git-common-dir) || exit 1
primary=$(cd "$(dirname "$common")" && pwd -P)
here=$(cd "$(git rev-parse --show-toplevel)" && pwd -P)
me=$(git config --get susi.agent 2>/dev/null || true)

git fetch --quiet --prune origin '+refs/heads/*:refs/remotes/origin/*' '+refs/claims/*:refs/claims/*' || {
    echo "❌ cannot reach origin: refusing to guess which worktrees are finished" >&2
    exit 1
}

now=$(date +%s)
active_branches=$(
    while IFS= read -r ref; do
        [ -n "$ref" ] || continue
        body=$(git cat-file -p "$ref" 2>/dev/null || true)
        if command -v jq >/dev/null 2>&1; then
            jq -r --argjson now "$now" 'select((.lease_until_unix // 0) > $now) | .branch // empty' <<<"$body" 2>/dev/null || true
        fi
    done < <(git for-each-ref --format='%(refname)' refs/claims/ 2>/dev/null || true)
)

kept=0
reclaimed=0
others=0
failed=0

while IFS= read -r wt; do
    [ -n "$wt" ] || continue
    git_path=$wt
    case "${OSTYPE:-}" in
        msys*|cygwin*) git_path=$(cygpath -aw "$wt" 2>/dev/null) || git_path=$wt ;;
    esac
    path=$(cd "$wt" 2>/dev/null && pwd -P) || {
        echo "  missing directory    $wt (run git worktree prune)"
        continue
    }
    if [ "$path" = "$primary" ]; then
        echo "  primary              $path"
        continue
    fi
    if [ "$path" = "$here" ]; then
        echo "  current              $path"
        continue
    fi
    head=$(git -C "$git_path" rev-parse HEAD 2>/dev/null) || {
        echo "  unreadable           $path"
        kept=$((kept + 1))
        continue
    }
    branch=$(git -C "$git_path" symbolic-ref -q --short HEAD 2>/dev/null || true)
    if [ -n "$branch" ] && printf '%s\n' "$active_branches" | grep -Fxq "$branch"; then
        echo "  active claim         $path"
        kept=$((kept + 1))
        continue
    fi
    if [ -n "$(git -C "$git_path" status --porcelain 2>/dev/null)" ]; then
        echo "  uncommitted changes  $path"
        kept=$((kept + 1))
        continue
    fi
    if ! git merge-base --is-ancestor "$head" origin/main 2>/dev/null; then
        echo "  unmerged commits     $path"
        kept=$((kept + 1))
        continue
    fi

    owner=$(git -C "$git_path" config --get susi.agent 2>/dev/null || true)
    mine=0
    if [ -n "$me" ] && [ "$owner" = "$me" ]; then
        mine=1
    fi
    if [ "$mine" = 0 ] && [ "$everything" = 0 ]; then
        echo "  another agent (${owner:-no owner})  $path"
        others=$((others + 1))
        continue
    fi

    if [ "$apply" = 0 ]; then
        echo "  reclaimable          $path"
        reclaimed=$((reclaimed + 1))
        continue
    fi
    if git worktree remove "$git_path" 2>/dev/null; then
        echo "  removed              $path"
        reclaimed=$((reclaimed + 1))
    else
        echo "  could not remove     $path" >&2
        failed=$((failed + 1))
    fi
done < <(git worktree list --porcelain | awk '/^worktree /{print $2}')

# Admin entries for directories that are already gone.
git worktree prune 2>/dev/null || true

if [ "$apply" = 0 ]; then
    echo "reclaimable: $reclaimed, other agents: $others, kept: $kept"
    if [ "$others" != 0 ]; then
        echo "  (--all reclaims another agent's finished worktree too)"
    fi
else
    echo "removed: $reclaimed, other agents: $others, kept: $kept, failed: $failed"
fi
[ "$failed" = 0 ]
