#!/usr/bin/env bash
# Reuse only a recent successful Windows job for this exact code tree.
# A successful workflow with a skipped Windows job is not Windows evidence.
set -euo pipefail
head=${1:?usage: ci-windows-proven.sh HEAD}
cutoff=$(date -u -d '24 hours ago' '+%Y-%m-%dT%H:%M:%SZ')
if ! candidates=$(gh run list --workflow test.yml --branch main --status success \
    --created ">=$cutoff" --limit 20 --json databaseId,headSha \
    --jq '.[] | [.databaseId, .headSha] | @tsv'); then
    exit 1
fi
while IFS=$'\t' read -r run sha; do
    [ -n "$run" ] && [ -n "$sha" ] || continue
    git cat-file -e "$sha^{commit}" 2>/dev/null || continue
    # --quiet returns nonzero both for a changed tree and a failed diff.
    git diff --quiet "$sha" "$head" -- ':(exclude).agents/tasks' || continue
    if result=$(gh run view "$run" --json jobs \
        --jq '[.jobs[] | select(.name == "Windows binary (release e2e checks)" and .conclusion == "success")] | length > 0') \
        && [ "$result" = true ]; then
        echo "Windows verification already passed for the same code tree in run $run"
        exit 0
    fi
done <<<"$candidates"
exit 1
