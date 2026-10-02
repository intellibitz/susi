#!/usr/bin/env bash
# Build output must never be tracked.
#
# debug/ (3390 files, 92 MB) and .rustc_info.json were committed by an unrelated
# `git add -A` on 2026-09-30. A tracked target directory is worse than bloat:
# the next build rewrites tracked files, so every worktree goes dirty for no
# reason, and a deletion commit like this one has to exist at all.
#
#   scripts/check-tracked-artifacts.sh          # exits 1 if build output is tracked
set -uo pipefail
root=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$root" || exit 0
patterns='^(debug|target|build|out|dist|node_modules|\.venv|__pycache__)/|^\.rustc_info\.json$|\.(pyc|rlib|rmeta)$'
bad=$(git ls-files | grep -E "$patterns" || true)
if [ -n "$bad" ]; then
    count=$(printf '%s\n' "$bad" | grep -c . || true)
    echo "❌ tracked build output: $count file(s)" >&2
    printf '%s\n' "$bad" | head -8 | sed 's/^/     /' >&2
    echo "   untrack with: git rm -r --cached <path>, then add it to .gitignore" >&2
    exit 1
fi
echo "✅ no build output is tracked ($(git ls-files | wc -l) file(s) in the index)"
