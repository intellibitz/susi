#!/usr/bin/env bash
# Session-start hook for agent tools (Claude Code SessionStart, or any wrapper):
# run the workflow check and print the result into the session, so the agent
# learns the rules and what to fix before it edits. It never blocks a session.
cd "$(git rev-parse --show-toplevel 2>/dev/null || pwd)" || exit 0
# A worktree the desktop app made has a private target/ and would cold-compile
# every dependency; share the clone's instead (a no-op where it already is, or
# where a build already lives in target/). See scripts/link-shared-target.sh.
[ -x scripts/link-shared-target.sh ] && scripts/link-shared-target.sh 2>&1
# Keep the primary checkout's main current (silent unless it advanced).
[ -x scripts/park-primary.sh ] && scripts/park-primary.sh --sync-only 2>/dev/null
if command -v susi >/dev/null 2>&1 && susi workflow --help >/dev/null 2>&1; then
    susi workflow check 2>&1 || true
elif [ -x ./target/debug/susi ]; then
    ./target/debug/susi workflow check 2>&1 || true
else
    echo "susi workflow check is not installed here; run: cargo run -q -- workflow check"
fi
cat <<'MSG'

Not ready? In the primary checkout or on main: scripts/susi-worktree.sh   (no name needed; it prints `cd <path>`)
Then: susi tasks list && susi tasks claim <id> --scope <path>, and end every commit with `Task: <id>`. Rules: AGENTS.md (START HERE).
MSG
exit 0
