#!/usr/bin/env bash
# Is the susi every agent runs the release built from this main?
#
# Agents drive the whole loop through `~/.susi/bin/susi`, and that binary
# changes only when a release is cut and promoted. A fix can therefore be merged
# and documented as active in AGENTS.md while the tool every worker runs still
# enforces the older rules: on 2026-10-02 the installed 0.21.0 was built from
# ee0f8269 — 92 commits behind main — and freed a claim taken on another branch,
# which main refuses. The version string cannot show this, because the release
# commit is what bumps it and every commit merged afterwards keeps it. Only the
# commit the promotion path recorded can, which is what this compares.
#
# Usage:
#   scripts/check-release-sync.sh     # 0 when the installed release is current
set -euo pipefail

# The promotion path installs to ${HOME}/.susi/bin regardless of an inherited
# SUSI_HOME (scripts/susi-release-sync.sh), so this reads the same root.
home="${HOME}/.susi"
marker="${home}/bin/susi.build.json"

if [ ! -f "$marker" ]; then
    echo "❌ no release is installed here ($marker absent)"
    echo "   → scripts/susi-release-sync.sh   # promotes the newest tag"
    exit 1
fi

commit=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("commit",""))' "$marker")
version=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("version","?"))' "$marker")
if [ -z "$commit" ]; then
    echo "❌ $marker names no commit — cannot tell which revision agents run"
    exit 1
fi

git fetch --quiet origin
if ! git merge-base --is-ancestor "$commit" origin/main 2>/dev/null; then
    echo "❌ the installed release ${version} was built from ${commit:0:8}, which is not on origin/main"
    echo "   → scripts/susi-release-sync.sh   # promotes a release built from main"
    exit 1
fi

# Only commits that changed the *code* count. The merge that lands a release
# adds no content, and a task close moves one file under .agents/tasks/ and adds
# none either — counting either made a release read as stale the moment it
# landed, or as soon as the next agent closed a task.
behind=$(git rev-list --no-merges --count "${commit}..origin/main" -- . ':(exclude).agents/tasks/*')
if [ "$behind" -gt 0 ]; then
    echo "❌ the installed release ${version} (${commit:0:8}) is ${behind} commit(s) behind origin/main"
    echo "   the workflow fixes merged since are not active for agents that run it"
    echo "   → cut one (susi admin release --cut patch), merge it, then scripts/susi-release-sync.sh"
    exit 1
fi

echo "✅ installed release ${version} (${commit:0:8}) is at origin/main"
