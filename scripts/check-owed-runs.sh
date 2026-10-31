#!/usr/bin/env bash
# Did a branch this agent handed to the merge machinery fail its run?
#
# `susi workflow finish` no longer waits for the branch-push run: it pushes,
# publishes the close receipt (refs/closed/<id>) and returns, and the merge
# machinery owns the rest. What the wait used to catch — a red run while the
# claim still made repair cheap — is caught here instead, at the next sync
# boundary every agent crosses, for the receipts this agent owes. Bounded by
# the pipeline (one owed merge, two at most) and one API call each. Advisory:
# it prints, it never blocks, and no `gh`, no `jq` or no answer is silence.
#
# The receipt records the head `tasks close` accepted, which is never the sha
# `finish` pushed: the close commit (and a merge of main) lands on top of it
# before the push, and the branch run is keyed on that pushed tip. So the run
# to read is the one for the tip of the remote branch that carries the receipt
# head — the branch `finish` pushed — and only a run for exactly that tip.
#
#   scripts/check-owed-runs.sh            # exit 0 always; ❌ lines name red runs
set -uo pipefail
[ ! -f "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh" ] || . "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh"
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
[ -f "$root/scripts/owed-task-receipts.sh" ] || exit 0

git fetch --quiet --prune "$remote" "+refs/heads/*:refs/remotes/$remote/*" '+refs/closed/*:refs/closed/*' 2>/dev/null || exit 0
while IFS= read -r id; do
    [ -n "$id" ] || continue
    body=$(git cat-file -p "refs/closed/$id" 2>/dev/null || true)
    head=$(jq -r '.head // ""' <<<"$body" 2>/dev/null)
    [ -n "$head" ] || continue
    # Published work: the close is on main (or the tested head itself is an
    # ancestor of main), the record just has not been cleaned up yet.
    git cat-file -e "$remote/main:.agents/tasks/done/$id.json" 2>/dev/null && continue
    if git merge-base --is-ancestor "$head" "$remote/main" 2>/dev/null \
        && ! git cat-file -e "$remote/main:.agents/tasks/$id.json" 2>/dev/null; then
        continue
    fi
    # Every remote branch (main aside) whose history holds the accepted head
    # is a branch finish pushed for this task; judge the run for its tip.
    while IFS= read -r ref; do
        [ -n "$ref" ] || continue
        case "$ref" in "refs/remotes/$remote/main" | "refs/remotes/$remote/HEAD") continue ;; esac
        tip=$(git rev-parse --verify --quiet "$ref" 2>/dev/null) || continue
        branch=${ref#refs/remotes/$remote/}
        state=$(gh run list --workflow Test --commit "$tip" --limit 5 \
            --json headSha,status,conclusion,url 2>/dev/null |
            jq -r --arg sha "$tip" \
                '[.[] | select(.headSha == $sha and .status == "completed")][0] | select(. != null) | .conclusion + " " + .url' \
                2>/dev/null || true)
        case "$state" in
        failure* | timed_out* | startup_failure*)
            cat >&2 <<MSG
❌ the branch run for $id ($branch at ${tip:0:12}) did not pass: ${state#* }
   The task is accepted but will not merge until this is fixed. Commit the
   repair on that branch and run \`susi workflow finish $id\` again (if the
   acceptance was already published, revert the close and fix under the same
   claim); or give it up on the record: susi tasks release $id --abandon <reason>
MSG
            ;;
        esac
    done < <(git for-each-ref --format='%(refname)' --contains "$head" "refs/remotes/$remote/" 2>/dev/null || true)
done < <("$root/scripts/owed-task-receipts.sh" "$agent" "$remote")
exit 0
