#!/usr/bin/env bash
# Reconcile every open PR so none sits in the queue forever. Runs every few
# minutes (auto-merge.yml) and covers what event-driven merging can miss: a
# dropped workflow_run, a green run that finished before the PR opened, a
# transient merge refusal.
#
#   reconcile-prs.sh <owner/repo>
#
# Per open PR (skipping wip/ nopr/ dependabot/ branches and drafts), judged on
# the latest Test run of its exact head sha (push- or dispatch-triggered):
#   green          -> merge it (auto-merge-pr.sh)
#   red            -> comment once with the run link; leave it open for a fix
#   running/none   -> wait
# A PR idle for STALE_DAYS that is not green is closed with a note (the branch
# is kept, and a later push reopens the flow).
set -uo pipefail
repo=${1:?repo}
here=$(cd "$(dirname "$0")" && pwd)
stale_days=${STALE_DAYS:-7}
marker='<!-- susi-reconcile -->'
now=$(date +%s)
rc=0

while IFS=$'\t' read -r num branch sha updated draft; do
    case "$branch" in wip/* | nopr/* | dependabot/*) continue ;; esac
    [ "$draft" = true ] && continue

    run=$(gh run list --repo "$repo" --workflow test.yml --commit "$sha" \
        --json status,conclusion,url --jq '.[0] // empty | [.status, .conclusion, .url] | @tsv' 2>/dev/null || true)
    status=$(printf '%s' "$run" | cut -f1)
    conclusion=$(printf '%s' "$run" | cut -f2)
    url=$(printf '%s' "$run" | cut -f3)

    if [ "$status" = completed ] && [ "$conclusion" = success ]; then
        echo "#$num green at ${sha:0:8}: merging"
        out=$("$here/auto-merge-pr.sh" "$repo" "$branch" "$sha" 2>&1) || rc=1
        printf '%s\n' "$out"
        # A green-but-behind head is re-synced, not merged — and every other
        # green PR is now stale relative to the merge this retest becomes.
        # Re-testing the whole queue at once burns a run per PR that the next
        # merge invalidates; rebase the queue one candidate at a time.
        case "$out" in *"was behind main; branch updated"*) break ;; esac
        continue
    fi

    if [ "$status" = completed ] && [ "$conclusion" = failure ]; then
        if ! gh pr view "$num" --repo "$repo" --json comments --jq '.comments[].body' 2>/dev/null | grep -qF "$marker $sha"; then
            gh pr comment "$num" --repo "$repo" --body "$marker $sha
The branch-push Test run failed for this commit: $url
Fix it and push; this PR merges itself once the run is green."
        fi
    fi

    idle_days=$(( (now - $(date -d "$updated" +%s 2>/dev/null || echo "$now")) / 86400 ))
    if [ "$idle_days" -ge "$stale_days" ]; then
        gh pr close "$num" --repo "$repo" --comment "$marker closing: idle for ${idle_days} days and not green. The branch is kept; push to it to start over." || true
    fi
done < <(gh pr list --repo "$repo" --base main --state open \
    --json number,headRefName,headRefOid,updatedAt,isDraft \
    --jq 'sort_by(.number) | .[] | [.number, .headRefName, .headRefOid, .updatedAt, .isDraft] | @tsv')
exit "$rc"
