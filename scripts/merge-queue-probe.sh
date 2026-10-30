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
[ ! -f "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh" ] || . "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh"
repo=${1:?repo}

# Succeeds unless main...sha answered that the head is current. A failed call
# used to read as "current" through the grep, so the probe answered `false`
# and the job installed nothing while the head it skipped was stale.
stale_green() { # sha
    local status
    status=$(gh api "repos/$repo/compare/main...$1" --jq .status 2>/dev/null) || return 0
    case "${status%$'\r'}" in
    ahead | identical) return 1 ;;
    *) return 0 ;;
    esac
}

if [ $# -ge 3 ]; then
    stale_green "$3" && echo true || echo false
    exit 0
fi

# Not a process substitution: its exit status is lost, and a failed listing
# read as an empty queue.
prs=$(gh pr list --repo "$repo" --base main --state open \
    --json number,headRefName,headRefOid,isDraft \
    --jq '.[] | [.number, .headRefName, .headRefOid, .isDraft] | @tsv' 2>/dev/null) \
    || { echo true; exit 0; }
green_count=0
while IFS=$'\t' read -r _num branch sha draft; do
    [ -n "$sha" ] || continue
    case "$branch" in wip/* | nopr/* | dependabot/*) continue ;; esac
    [ "$draft" = true ] && continue
    green=$(gh run list --repo "$repo" --workflow test.yml --commit "$sha" \
        --json status,conclusion \
        --jq '.[0] | select(.status=="completed" and .conclusion=="success") | "yes"' 2>/dev/null) \
        || { echo true; exit 0; }
    [ "${green%$'\r'}" = "yes" ] || continue
    stale_green "$sha" && { echo true; exit 0; }
    green_count=$((green_count + 1))
    # Two or more green PRs in the same sweep: merging the first advances main
    # and makes the second green-but-behind inside the same `reconcile` run.
    [ "$green_count" -ge 2 ] && { echo true; exit 0; }
done < <(printf '%s\n' "$prs" | tr -d '\r')

echo false
