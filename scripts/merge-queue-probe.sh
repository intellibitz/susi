#!/usr/bin/env bash
# Will the inline merge gate actually run? Prints `true` when an open PR has a
# green-but-behind head — the only state that makes the serialized merge job
# compile a merge tree — so the job can skip installing a toolchain, cache and
# system deps on the common pass where nothing is stale. False positives cost
# a compile; false negatives cost a queue round-trip, so any API failure
# answers `true`.
#
#   merge-queue-probe.sh <owner/repo>                # any open PR
#   merge-queue-probe.sh <owner/repo> <branch> <sha> # just this head
set -uo pipefail
repo=${1:?repo}

stale_green() { # sha
    gh api "repos/$repo/compare/main...$1" --jq .status 2>/dev/null \
        | grep -Eqx 'behind|diverged'
}

if [ $# -ge 3 ]; then
    stale_green "$3" && echo true || echo false
    exit 0
fi

while IFS=$'\t' read -r _num branch sha draft; do
    case "$branch" in wip/* | nopr/* | dependabot/*) continue ;; esac
    [ "$draft" = true ] && continue
    green=$(gh run list --repo "$repo" --workflow test.yml --commit "$sha" \
        --json status,conclusion \
        --jq '.[0] | select(.status=="completed" and .conclusion=="success") | "yes"' 2>/dev/null || true)
    [ "$green" = "yes" ] || continue
    stale_green "$sha" && { echo true; exit 0; }
done < <(gh pr list --repo "$repo" --base main --state open \
    --json number,headRefName,headRefOid,isDraft \
    --jq '.[] | [.number, .headRefName, .headRefOid, .isDraft] | @tsv' 2>/dev/null || true)

echo false
