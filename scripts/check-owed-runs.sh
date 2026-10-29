#!/usr/bin/env bash
# Did a branch this agent handed to the merge machinery fail its run?
#
# `susi workflow finish` no longer waits for the branch-push run: it pushes,
# publishes the close receipt (refs/closed/<id>) and returns, and the merge
# machinery owns the rest. What the wait used to catch — a red run while the
# claim still made repair cheap — is caught here instead, at the next sync
# boundary every agent crosses, for the receipts this agent owes: the run for
# the receipt's head, when `gh` can read it. Bounded by the pipeline (one owed
# merge, two at most) and one API call each. Advisory: it prints, it never
# blocks, and no `gh`, no `jq` or no answer is silence.
#
#   scripts/check-owed-runs.sh            # exit 0 always; ❌ lines name red runs
set -uo pipefail
root=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$root" || exit 0
command -v gh >/dev/null 2>&1 || exit 0
command -v jq >/dev/null 2>&1 || exit 0
# The agent token as the binary derives it: SUSI_AGENT first, else the
# worktree's susi.agent, uppercased with anything but letters and digits
# dropped (`codex-1` and `codex1` are both CODEX1).
agent=${SUSI_AGENT:-$(git config --get susi.agent 2>/dev/null || true)}
agent=$(printf '%s' "$agent" | tr '[:lower:]' '[:upper:]' | tr -cd 'A-Z0-9')
[ -n "$agent" ] || exit 0
remote=${SUSI_TASK_REMOTE:-origin}

git fetch --quiet --prune "$remote" '+refs/closed/*:refs/closed/*' 2>/dev/null || exit 0
while IFS= read -r name; do
    [ -n "$name" ] || continue
    id=${name#refs/closed/}
    body=$(git cat-file -p "$name" 2>/dev/null || true)
    [ "$(jq -r '.agent // ""' <<<"$body" 2>/dev/null)" = "$agent" ] || continue
    head=$(jq -r '.head // ""' <<<"$body" 2>/dev/null)
    [ -n "$head" ] || continue
    # Published work: the close is on main, the record just has not been
    # cleaned up yet.
    git cat-file -e "$remote/main:.agents/tasks/done/$id.json" 2>/dev/null && continue
    state=$(gh run list --workflow Test --commit "$head" --limit 5 \
        --json status,conclusion,url 2>/dev/null |
        jq -r '[.[] | select(.status == "completed")][0] | select(. != null) | .conclusion + " " + .url' 2>/dev/null || true)
    case "$state" in
    failure* | timed_out* | startup_failure*)
        cat >&2 <<MSG
❌ the branch run for $id (head ${head:0:12}) did not pass: ${state#* }
   The task is accepted but will not merge until this is fixed. Commit the
   repair on that branch and run \`susi workflow finish $id\` again (if the
   acceptance was already published, revert the close and fix under the same
   claim); or give it up on the record: susi tasks release $id --abandon <reason>
MSG
        ;;
    esac
done < <(git for-each-ref --format='%(refname)' refs/closed/ 2>/dev/null || true)
exit 0
