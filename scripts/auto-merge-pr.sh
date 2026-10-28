#!/usr/bin/env bash
# Merge the open PR for <branch> if its head is exactly <sha>, and say why when
# it cannot be merged. Used by every path that can decide "this is green":
# the workflow_run merge job, the open-pr job (Test may finish before the PR
# exists) and the reconciler. Idempotent and safe to call repeatedly.
#
#   auto-merge-pr.sh <owner/repo> <branch> <sha>
# exit 0: merged, nothing to do (no such open PR / head moved), the head
#         was re-synced to main (update-branch) and the retest will merge it,
#         or (SUSI_NO_RESYNC=1) the stale head was handed to a reconcile pass
# exit 1: the merge was refused (a comment explains, once per head sha)
#
# A green head must include the latest integration branch, not merely have
# passed against an older base. Serialization means every merge turns the next
# green head stale. When SUSI_INLINE_MERGE_GATE=1 (the integration jobs set it
# when their probe installed the gate's toolchain) the stale head is not sent
# through an update-branch + retest round-trip — minutes of queue latency per
# merge: the script builds the merge locally (`git merge-tree`), runs the same
# compile gate the `check` job runs (merge-gate.sh) on the merge tree, and
# merges the PR when it passes. The merge commit GitHub produces has that exact
# tree for a clean merge, so the gated bytes are the bytes that land. A gate
# failure or a conflict is explained once per head sha; an environment that
# cannot run the gate falls back to the resync-and-retest path.
#
# SUSI_NO_RESYNC=1 is the retest's own merge job (test.yml `merge`): that run
# is the retest a resync dispatched, so a resync from there dispatches another
# retest of the same PR, and one more each time main moves while it runs. When
# it cannot gate a stale head it hands the head to a reconcile pass instead.
set -uo pipefail
[ ! -f "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh" ] || . "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh"
repo=${1:?repo} branch=${2:?branch} sha=${3:?sha}
marker='<!-- susi-auto-merge -->'
here=$(cd "$(dirname "$0")" && pwd)

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
# directly instead of waiting for the main run's `verify` job to recompute a
# known answer. This attests the acceptance only, never the suite: the branch
# run compiles and lints, so the full suite is dispatched after every merge
# whatever this returns. A failed ref push returns non-zero so the `verify`
# job still gets to record it.
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

# Merge the PR at the tested head, attest, dispatch the main suite. When the
# caller has already gated a local merge tree ($gated_tree set), the merge
# commit GitHub produced is compared against it: a clean merge produces that
# exact tree, and a difference means the bytes that landed are not the bytes
# that were proven — worth a line, never a failure (the main suite decides).
merge_head() {
    if ! gh pr merge "$pr" --repo "$repo" --merge --match-head-commit "$sha"; then
        return 1
    fi
    echo "merged #$pr"
    merge_sha=$(gh pr view "$pr" --repo "$repo" --json mergeCommit \
        --jq '.mergeCommit.oid' 2>/dev/null || true)
    [ -n "${merge_sha:-}" ] && attest_merge "$pr" "$merge_sha"
    if [ -n "${merge_sha:-}" ] && same_tree "$merge_sha" "$sha" \
        && attest_verified "$pr" "$merge_sha"; then
        echo "merge tree identical to the tested head — acceptance attested by identity"
    fi
    if [ -n "${gated_tree:-}" ] && [ -n "${merge_sha:-}" ]; then
        git fetch --quiet origin "$merge_sha" 2>/dev/null || true
        if [ "$(git rev-parse "$merge_sha^{tree}" 2>/dev/null)" = "$gated_tree" ]; then
            echo "server merge tree identical to the gated merge tree"
        else
            echo "note: the merge commit's tree differs from the gated merge tree — the main suite is the proof" >&2
        fi
    fi
    # GITHUB_TOKEN merges do not trigger workflows: run the full suite on main.
    gh workflow run test.yml --repo "$repo" --ref main || echo "note: could not dispatch the main suite" >&2
    # Main moved, so every other green PR may now be mergeable (or need its
    # behind head gated). Reconcile now instead of at the next scheduled pass,
    # which GitHub throttles to hours. A pass that merges dispatches the next
    # one, and a pass with nothing to merge dispatches nothing, so the queue
    # drains and stops. A pass that is already running is the reconciler.
    if [ "${SUSI_RECONCILING:-0}" != 1 ]; then
        gh workflow run auto-merge.yml --repo "$repo" --ref main \
            || echo "note: could not dispatch a reconcile pass" >&2
    fi
    return 0
}

# Prove a behind/diverged head's merge with main locally.
#   0  merge tree built and gated — merge the PR
#   1  this environment cannot run the gate — caller falls back to update-branch
#   2  the merge conflicts or fails the gate — `reason` is set, fail the run
gated_tree=
inline_merge_gate() {
    command -v git >/dev/null 2>&1 || return 1
    command -v cargo >/dev/null 2>&1 || return 1
    # The runner image ships cargo, so cargo alone does not say the job set
    # the gate up. A reconcile pass whose probe skipped the toolchain still
    # had RUSTC_WRAPPER=sccache in its job env; clippy failed with "could not
    # execute process `sccache ...`" and the PR was told its merge "fails the
    # compile gate" when nothing was wrong with it. A wrapper that does not
    # resolve is an environment that cannot gate, not a red gate.
    local wrapper
    for wrapper in "${RUSTC_WRAPPER:-}" "${RUSTC_WORKSPACE_WRAPPER:-}"; do
        [ -z "$wrapper" ] || command -v "$wrapper" >/dev/null 2>&1 || return 1
    done
    git rev-parse --verify --quiet origin/main >/dev/null 2>&1 || return 1
    git fetch --quiet origin main "$sha" || return 1
    local base tree merge_commit
    base=$(git rev-parse origin/main) || return 1
    if ! tree=$(git merge-tree --write-tree origin/main "$sha" 2>/dev/null); then
        reason="the branch conflicts with main — merge origin/main into it, resolve, and push (this re-runs automatically)"
        return 2
    fi
    # A CI runner has no git identity and commit-tree refuses without one
    # ("Author identity unknown"), so the gate fell back to resync-and-retest
    # for every behind PR instead of merging it. This commit stays local and is
    # never pushed: any fixed identity will do.
    merge_commit=$(GIT_AUTHOR_NAME="susi merge gate" GIT_AUTHOR_EMAIL="merge-gate@susi.invalid" \
        GIT_COMMITTER_NAME="susi merge gate" GIT_COMMITTER_EMAIL="merge-gate@susi.invalid" \
        git commit-tree "$tree" -p origin/main -p "$sha" \
        -m "merge-gate: $branch onto ${base:0:12}") || return 1
    # Gate in place: this checkout is disposable, and its warm target dir and
    # fingerprints are what make the proof a bounded compile.
    git checkout --quiet -f --detach "$merge_commit" || return 1
    if "${SUSI_MERGE_GATE:-$here/merge-gate.sh}" "$base"; then
        gated_tree=$tree
        return 0
    fi
    reason="the merge with current main fails the compile gate — merge origin/main into the branch, fix it, and push (this re-runs automatically)"
    return 2
}

pr=$(gh pr list --repo "$repo" --head "$branch" --base main --state open \
    --json number,headRefOid --jq ".[] | select(.headRefOid==\"$sha\") | .number" | head -1)
if [ -z "$pr" ]; then
    echo "no open PR whose head is $sha (stale run, or none yet)"
    exit 0
fi

comparison=$(gh api "repos/$repo/compare/main...$sha" --jq .status) || exit 1
reason=
case "$comparison" in
 ahead|identical)
    if merge_head; then
        exit 0
    fi
    state=$(gh pr view "$pr" --repo "$repo" --json mergeable --jq .mergeable 2>/dev/null || echo UNKNOWN)
    case "$state" in
    CONFLICTING) reason="the branch conflicts with main — merge origin/main into it, resolve, and push (this re-runs automatically)" ;;
    *) reason="GitHub refused the merge (mergeable=$state) — if the head moved, the next push retries" ;;
    esac
    ;;
 *)
    resync() {
        # The retest's merge job never resyncs: its run is the retest, and
        # every resync from it dispatched the next one, so a PR whose retest
        # kept falling behind looped for as long as main kept moving — a
        # branch gate and a merge commit per lap, never merged. A reconcile
        # pass judges the completed green head with the inline gate.
        if [ "${SUSI_NO_RESYNC:-0}" = 1 ]; then
            gh workflow run auto-merge.yml --repo "$repo" --ref main \
                || echo "note: could not dispatch a reconcile pass; the scheduled pass merges it" >&2
            echo "PR #$pr fell behind main during its retest; not resyncing again — a reconcile pass gates its merge."
            exit 0
        fi
        # A token-made branch update raises no push event, so the gate would
        # never see the new head. Dispatch it explicitly; the dispatched run
        # merges the head itself once green (test.yml `merge`), because its
        # completion raises no workflow_run either.
        if gh pr update-branch "$pr" --repo "$repo"; then
            # The branch gate, as a push would have run it: not the full
            # suite, which takes 20+ minutes and fails on flakes no branch
            # push runs.
            gh workflow run test.yml --repo "$repo" --ref "$branch" -f gate=branch || true
            echo "PR #$pr was behind main; branch updated — its retest merges it."
            exit 0
        fi
        reason="the branch is behind main and could not be updated — merge origin/main into it, resolve the conflict, and push"
    }
    if [ "${SUSI_INLINE_MERGE_GATE:-0}" = 1 ]; then
        inline_merge_gate
        case $? in
        0)
            if merge_head; then
                exit 0
            fi
            state=$(gh pr view "$pr" --repo "$repo" --json mergeable --jq .mergeable 2>/dev/null || echo UNKNOWN)
            reason="GitHub refused the merge of the gated merge tree (mergeable=$state) — if the head moved, the next push retries"
            ;;
        1) resync ;; # no checkout or toolchain here — the retest path, or a reconcile pass
        2) ;;        # conflict or a red gate: `reason` is set
        esac
    else
        resync
    fi
    ;;
esac
# One comment per head sha: a retrying reconciler must not spam the PR.
if ! gh pr view "$pr" --repo "$repo" --json comments --jq '.comments[].body' 2>/dev/null | grep -qF "$marker $sha"; then
    gh pr comment "$pr" --repo "$repo" --body "$marker $sha
Auto-merge could not complete: $reason."
fi
exit 1
