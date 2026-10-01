#!/usr/bin/env bash
# Attribute a red `main` to the merge that caused it.
#
# With several agents merging in parallel, "main is red" is a shared signal with
# no owner: the failing commit is a *merge* commit, and nothing links it back to
# the pull request whose work it carries, so every agent bisects the same
# failure to find out whether it is theirs. This finds the pull request whose
# merge produced that sha and comments once.
#
#   scripts/report-main-failure.sh <main-sha> [run-url]
#
# Idempotent: the marker is keyed by the sha, so a retried job or a re-run does
# not comment twice. Silent and successful when the sha cannot be attributed —
# an unattributable red run is still a red run, but not this script's error.
set -uo pipefail

sha=${1:?usage: report-main-failure.sh <main-sha> [run-url]}
url=${2:-}
marker="<!-- susi-main-failure:${sha} -->"

if ! command -v gh >/dev/null 2>&1 || ! command -v jq >/dev/null 2>&1; then
    echo "gh or jq unavailable: cannot attribute $sha" >&2
    exit 0
fi

pr=$(gh pr list --state merged --limit 100 --json number,mergeCommit 2>/dev/null |
    jq -r --arg sha "$sha" '[.[] | select(.mergeCommit.oid == $sha)][0].number // empty' 2>/dev/null || true)
if [ -z "$pr" ]; then
    echo "no merged pull request records $sha as its merge commit" >&2
    exit 0
fi

if gh pr view "$pr" --json comments 2>/dev/null |
    jq -r '[.comments[].body] | join("\n")' 2>/dev/null | grep -qF "$marker"; then
    echo "already reported on #$pr"
    exit 0
fi

# Built without a heredoc or backticks on purpose: prose that names a branch is
# exactly where unquoted expansions bite.
body="${marker}
❌ the full suite failed on main for this merge."
if [ -n "$url" ]; then
    body="${body}
Run: ${url}"
fi
body="${body}

Mandate 54: a red run is a defect. The jobs that run only on main are the ones
that could not run before the merge, so a failure here is usually the first
sight of it. Fix forward on a new branch — never force-push main — and name this
merge in the task that carries the fix."

gh pr comment "$pr" --body "$body"
echo "commented on #$pr"
