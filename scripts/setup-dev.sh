#!/usr/bin/env bash
# One-time per clone (shared by every worktree of it): enable the repo's git
# hooks and the ledger merge driver so parallel agents converge cleanly.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
git config core.hooksPath .githooks
git config merge.ledger.name "union of appended .agents/evidence.json entries"
git config merge.ledger.driver "python3 scripts/merge-ledger.py %O %A %B"
echo "hooks: $(git config core.hooksPath); merge driver: ledger"
