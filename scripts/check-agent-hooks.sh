#!/usr/bin/env bash
# Every agent tool's session-start hook config must be tracked, or worktrees never get it.
cd "$(git rev-parse --show-toplevel)" || exit 1
rc=0
for f in .claude/settings.json .codex/hooks.json; do
    git ls-files --error-unmatch "$f" >/dev/null 2>&1 || { echo "not tracked: $f"; rc=1; }
done
exit $rc
