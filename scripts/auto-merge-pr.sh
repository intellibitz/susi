#!/usr/bin/env bash
# Merge the open PR for <branch> if its head is exactly <sha>, and say why when
# it cannot be merged. Used by every path that can decide "this is green":
# the workflow_run merge job, the open-pr job (Test may finish before the PR
# exists) and the reconciler. Idempotent and safe to call repeatedly.
#
#   auto-merge-pr.sh <owner/repo> <branch> <sha>
# exit 0: merged, or nothing to do (no such open PR / head moved)
# exit 1: the merge was refused (a comment explains, once per head sha)
set -uo pipefail
repo=${1:?repo} branch=${2:?branch} sha=${3:?sha}
marker='<!-- susi-auto-merge -->'

pr=$(gh pr list --repo "$repo" --head "$branch" --base main --state open \
    --json number,headRefOid --jq ".[] | select(.headRefOid==\"$sha\") | .number" | head -1)
if [ -z "$pr" ]; then
    echo "no open PR whose head is $sha (stale run, or none yet)"
    exit 0
fi

# A green head must include the latest integration branch, not merely have
# passed against an older base. The agent syncs, retests and pushes on refusal.
comparison=$(gh api "repos/$repo/compare/main...$sha" --jq .status) || exit 1
case "$comparison" in
 ahead|identical) ;;
 *) echo "PR #$pr is behind or diverged from main; sync and retest before merge." >&2; exit 1 ;;
esac

if gh pr merge "$pr" --repo "$repo" --merge --match-head-commit "$sha"; then
    echo "merged #$pr"
    # GITHUB_TOKEN merges do not trigger workflows: run the full suite on main.
    gh workflow run test.yml --repo "$repo" --ref main || echo "note: could not dispatch the main suite" >&2
    exit 0
fi

state=$(gh pr view "$pr" --repo "$repo" --json mergeable --jq .mergeable 2>/dev/null || echo UNKNOWN)
case "$state" in
CONFLICTING) reason="the branch conflicts with main — merge origin/main into it, resolve, and push (this re-runs automatically)" ;;
*) reason="GitHub refused the merge (mergeable=$state) — if the head moved, the next push retries" ;;
esac
# One comment per head sha: a retrying reconciler must not spam the PR.
if ! gh pr view "$pr" --repo "$repo" --json comments --jq '.comments[].body' 2>/dev/null | grep -qF "$marker $sha"; then
    gh pr comment "$pr" --repo "$repo" --body "$marker $sha
Auto-merge could not complete: $reason."
fi
exit 1
