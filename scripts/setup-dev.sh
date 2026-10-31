#!/usr/bin/env bash
# One-time per clone (shared by every worktree of it): enable the repo's git
# hooks and the ledger merge driver so parallel agents converge cleanly.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
git config core.hooksPath .githooks
git config merge.ledger.name "union of appended .agents/evidence.json entries"
git config merge.ledger.driver "sh scripts/merge-ledger.sh %O %A %B"
git config merge.roadmap.name "per-vector merge of .agents/roadmap.json"
git config merge.roadmap.driver "sh scripts/merge-roadmap.sh %O %A %B"
echo "hooks: $(git config core.hooksPath); merge drivers: ledger, roadmap"
