#!/usr/bin/env bash
# Merge the open PR for <branch> if its head is exactly <sha>, and say why when
# it cannot be merged. Used by every path that can decide "this is green":
# the workflow_run merge job, the open-pr job (Test may finish before the PR
# exists) and the reconciler. Idempotent and safe to call repeatedly.
#
#   auto-merge-pr.sh <owner/repo> <branch> <sha>
# exit 0: merged, nothing to do (no such open PR / head moved), or the head
#         was re-synced to main (update-branch) and the retest will merge it
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
# passed against an older base. Serialization means every merge turns the
# next green head stale, so "behind" is a routine queue state, not a defect:
# re-sync the branch server-side (update-branch re-runs the push gate and the
# next green run merges) instead of failing the run and waiting on the agent.
comparison=$(gh api "repos/$repo/compare/main...$sha" --jq .status) || exit 1
reason=
case "$comparison" in
 ahead|identical)
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
    ;;
 *)
    if gh pr update-branch "$pr" --repo "$repo"; then
        # A token-made branch update raises no push event, so the gate would
        # never see the new head. Dispatch it explicitly; the next reconcile
        # pass (or a workflow_run hook) merges it once green.
        gh workflow run test.yml --repo "$repo" --ref "$branch" || true
        echo "PR #$pr was behind main; branch updated — its retest merges it."
        exit 0
    fi
    reason="the branch is behind main and could not be updated — merge origin/main into it, resolve the conflict, and push"
    ;;
esac
# One comment per head sha: a retrying reconciler must not spam the PR.
if ! gh pr view "$pr" --repo "$repo" --json comments --jq '.comments[].body' 2>/dev/null | grep -qF "$marker $sha"; then
    gh pr comment "$pr" --repo "$repo" --body "$marker $sha
Auto-merge could not complete: $reason."
fi
exit 1
