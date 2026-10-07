#!/usr/bin/env bash
# ci-code-proven.sh <head> [sha ...] — exit 0 when <head>'s code is identical
# to one of the given commits.
#
# "Code" is every path except .agents/: the swarm's task records, close
# receipts and roadmap narratives churn on nearly every merge and can never
# change a test verdict, while everything else — workflows, scripts, the
# lockfile, vendor, .cargo/ — can. A merge that moves only .agents/ therefore
# lands on a main tree whose suite result already exists, and re-running the
# shards for ~20-30 min re-proves proven bytes while every later merge queues
# behind it (the main concurrency group keeps one run and one pending).
#
# Callers hand in the shas of runs that already concluded green; a sha that
# does not resolve locally is simply not evidence, never an error, and no
# match means the suite runs — the default is always to prove, not to skip.
set -euo pipefail

head=${1:?usage: ci-code-proven.sh <head> [green-sha ...]}
shift

for sha in "$@"; do
    git cat-file -e "$sha^{commit}" 2>/dev/null || continue
    if [ -z "$(git diff --name-only "$sha" "$head" -- ':(exclude).agents')" ]; then
        exit 0
    fi
done
exit 1
