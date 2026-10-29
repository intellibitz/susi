#!/usr/bin/env bash
# ci-code-proven.sh <head> [sha ...] — exit 0 when <head>'s code is identical
# to one of the given commits.
#
# "Code" is every path except .agents/tasks/, the exemption ci-changed-crates.sh
# makes too: the task queue (one file per task, and the done records closes
# write) churns on nearly every merge and is not a build input, while
# everything else can change a verdict — workflows, scripts, the lockfile,
# vendor, .cargo/, and the rest of .agents/: build.rs compiles identity, roadmap
# and evidence.json, and root tests read roadmap.json and roadmap-verdicts/. A
# push that moved only the queue therefore lands on a main tree whose suite
# result already exists, and re-running the shards re-proves proven bytes while
# every later merge queues behind it; one that moved the roadmap runs them.
#
# Callers hand in the shas of runs that already concluded green; a sha that
# does not resolve locally is simply not evidence, never an error, and no
# match means the suite runs — the default is always to prove, not to skip.
set -euo pipefail

head=${1:?usage: ci-code-proven.sh <head> [green-sha ...]}
shift

for sha in "$@"; do
    git cat-file -e "$sha^{commit}" 2>/dev/null || continue
    if [ -z "$(git diff --name-only "$sha" "$head" -- ':(exclude).agents/tasks')" ]; then
        exit 0
    fi
done
exit 1
