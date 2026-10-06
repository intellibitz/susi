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

# Attest the merge on the shared remote: for every task the pull request named,
# push refs/merged/<task> = {task, head, merge, pr, merged_unix}. This job is
# the only party that observes the merge, so it is the only one that can say
# "the tested head is on main" without taking the agent's word for it — and
# `susi tasks audit` re-derives every claim from origin/main rather than
# trusting the ref. Best effort: the merge has already happened, and a missing
# attestation costs one audit line, never a merge.
attest_merge() {
    local pr=$1 merge_sha=$2 messages task body blob
    messages=$(gh api "repos/$repo/pulls/$pr/commits" --paginate \
        --jq '.[].commit.message' 2>/dev/null) || return 0
    for task in $(printf '%s\n' "$messages" \
        | sed -n 's/^Task: *\(T-[A-Z0-9]*-[0-9]*\)$/\1/p' | sort -u); do
        body=$(printf '{"task":"%s","head":"%s","merge":"%s","pr":%s,"merged_unix":%s}' \
            "$task" "$sha" "$merge_sha" "$pr" "$(date +%s)")
        blob=$(printf '%s' "$body" | git hash-object -w --stdin) || continue
        # The empty lease expects the ref to be absent: one task merges once,
        # and a ref already there belongs to an earlier merge of the same id.
        if git push --quiet "--force-with-lease=refs/merged/$task:" \
            origin "$blob:refs/merged/$task" 2>/dev/null; then
            echo "attested $task merged as $merge_sha"
        else
            echo "note: could not attest $task on refs/merged/$task" >&2
        fi
    done
    return 0
}

# Whether commit $1's tree is byte-identical to commit $2's. A merge whose
# branch already contained main only gains a parent, so the merge commit's
# tree is the tested head's tree — the green branch run already proved these
# exact bytes, byte for byte.
same_tree() {
    gh api "repos/$repo/compare/$1..$2" --jq .status 2>/dev/null | grep -qx identical
}

# A tree-identical merge makes the post-merge acceptance re-run provable by
# transitivity: `close` ran it on this very tree. Write refs/verified/<task>
# directly instead of dispatching a second full suite to recompute a known
# answer. Doubt is never silently skipped: unknown task set or a failed ref
# push returns non-zero and the caller dispatches the real suite.
attest_verified() {
    local pr=$1 merge_sha=$2 messages task body blob rc=0
    messages=$(gh api "repos/$repo/pulls/$pr/commits" --paginate \
        --jq '.[].commit.message' 2>/dev/null) || return 1
    for task in $(printf '%s\n' "$messages" \
        | sed -n 's/^Task: *\(T-[A-Z0-9]*-[0-9]*\)$/\1/p' | sort -u); do
        body=$(printf '{"task":"%s","head":"%s","merge":"%s","result":"passed","verified_unix":%s}' \
            "$task" "$sha" "$merge_sha" "$(date +%s)")
        blob=$(printf '%s' "$body" | git hash-object -w --stdin) || { rc=1; continue; }
        # The empty lease expects the ref to be absent, as with refs/merged:
        # a verification already recorded stands.
        if git push --quiet "--force-with-lease=refs/verified/$task:" \
            origin "$blob:refs/verified/$task" 2>/dev/null; then
            echo "verified $task by tree identity (merge $merge_sha == tested head $sha)"
        else
            echo "note: could not record verification of $task" >&2
            rc=1
        fi
    done
    return $rc
}

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
        merge_sha=$(gh pr view "$pr" --repo "$repo" --json mergeCommit \
            --jq '.mergeCommit.oid' 2>/dev/null || true)
        [ -n "${merge_sha:-}" ] && attest_merge "$pr" "$merge_sha"
        # The common case is a merge whose tree is byte-identical to the tested
        # head's: `finish` integrates origin/main before pushing, so merging a
        # branch that is current only adds a parent. The branch run already
        # proved those exact bytes — attest the post-merge acceptance by
        # identity instead of dispatching the full main suite to recompute it.
        # A merge that raced another (different tree) still gets the suite.
        if [ -n "${merge_sha:-}" ] && same_tree "$merge_sha" "$sha" \
            && attest_verified "$pr" "$merge_sha"; then
            echo "merge tree identical to the tested head — green run already proved it; main suite skipped"
        else
            # GITHUB_TOKEN merges do not trigger workflows: run the full suite on main.
            gh workflow run test.yml --repo "$repo" --ref main || echo "note: could not dispatch the main suite" >&2
        fi
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
