#!/usr/bin/env bash
# Publish accepted code and release ownership in one compare-and-swap push.
#
# Exit 75 requests the existing separate queue path (cloud relay, another queue
# remote, or an already released claim). Other failures never fall back or release.
set -euo pipefail

die() {
    echo "workflow: $*" >&2
    exit 1
}

[ "$#" -eq 2 ] || die "usage: publish-task-branch.sh <task> <branch>"
task=$1
branch=$2

if [[ ! "$task" =~ ^T-[A-Z0-9]+-[0-9]+$ ]] || [ -z "$branch" ] || [ "$branch" = "main" ] || [ "$branch" = "master" ]; then
    die "publication requires a task and its feature branch"
fi
if [ "${SUSI_TASK_REMOTE:-origin}" != "origin" ]; then
    exit 75
fi
transport=$(printf '%s' "${SUSI_QUEUE_TRANSPORT:-}" | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')
if [ "$transport" = "relay" ]; then
    exit 75
fi

cfg_cloud=$(git config --get susi.cloud 2>/dev/null || echo "false")
cfg_cloud=$(printf '%s' "$cfg_cloud" | tr '[:upper:]' '[:lower:]')
cloud=0
case "$cfg_cloud" in
true | yes | on | 1) cloud=1 ;;
esac
explicit=$(printf '%s' "${SUSI_CLOUD_SESSION:-}" | tr '[:upper:]' '[:lower:]')
case "$explicit" in
true | yes | on | 1) cloud=1 ;;
false | no | off | 0) cloud=0 ;;
esac
if [ "$cloud" -eq 1 ]; then
    exit 75
fi

raw_agent=${SUSI_AGENT:-$(git config --get susi.agent 2>/dev/null || true)}
agent=$(printf '%s' "$raw_agent" | tr '[:lower:]' '[:upper:]' | tr -cd 'A-Z0-9')
[ -n "$agent" ] || die "publication requires this worktree's identity"

here=$(git symbolic-ref --quiet --short HEAD 2>/dev/null) || die "cannot resolve current branch"
[ "$here" = "$branch" ] || die "the branch changed before publication"
head=$(git rev-parse HEAD 2>/dev/null) || die "cannot resolve HEAD"

claim_ref="refs/claims/$task"
agent_ref="refs/claim-agents/$agent"
receipt_ref="refs/closed/$task"
done_ref="$head:.agents/tasks/done/$task.json"

claim_sha=$(git rev-parse --verify --quiet "$claim_ref" 2>/dev/null || true)
if [ -z "$claim_sha" ]; then
    exit 75
fi
claim_body=$(git cat-file -p "$claim_sha" 2>/dev/null) || die "invalid workflow blob: $claim_ref"

index_sha=$(git rev-parse --verify --quiet "$agent_ref" 2>/dev/null || true)
index_body=null
if [ -n "$index_sha" ]; then
    index_body=$(git cat-file -p "$index_sha" 2>/dev/null) || die "invalid workflow blob: $agent_ref"
fi

receipt_sha=$(git rev-parse --verify --quiet "$receipt_ref" 2>/dev/null || true)
done_sha=$(git rev-parse --verify --quiet "$done_ref" 2>/dev/null || true)
[ -n "$done_sha" ] || die "publication requires the committed completion"
done_body=$(git cat-file -p "$done_sha" 2>/dev/null) || die "invalid workflow blob: $done_ref"
receipt_body=null
if [ -n "$receipt_sha" ]; then
    receipt_body=$(git cat-file -p "$receipt_sha" 2>/dev/null) || die "invalid workflow blob: $receipt_ref"
fi

# Validate ownership, completion, and the accepted receipt in one jq process.
# The four blobs travel on stdin, not in command-line arguments: a claim can
# have many scopes and Windows limits the command-line length. The output is two
# lines: the accepted head, then compact JSON for the replacement receipt.
now=$(date +%s)
parsed=$(printf '%s\n%s\n%s\n%s\n' "$claim_body" "$index_body" "$done_body" "$receipt_body" |
    command jq -rcS -s --arg task "$task" --arg agent "$agent" --arg branch "$branch" \
        --arg head "$head" --arg has_index "$index_sha" --arg has_receipt "$receipt_sha" \
        --argjson now "$now" '
        def owned: .task == $task and .agent == $agent and (.branch == null or .branch == $branch);
        if length != 4 then error("invalid workflow records") else . end |
        .[0] as $claim | .[1] as $index | .[2] as $done | .[3] as $old |
        if ($claim | type) != "object" or
           ($has_index != "" and ($index | type) != "object") or
           ($done | type) != "object" or ($done.closed | type) != "object" or
           ($has_receipt != "" and ($old | type) != "object")
        then error("invalid workflow record") else . end |
        if ($claim | owned | not) or ($has_index != "" and ($index | owned | not))
        then error("ownership changed before publication; claim retained") else . end |
        (if $has_receipt == "" then
            {task: $task, agent: $agent, head: $done.closed.commit, closed_unix: ($done.closed.at_unix // 0)}
            + (if ($claim.scopes | type) == "array" and ($claim.scopes | length) > 0
               then {scopes: $claim.scopes} else {} end)
         else $old end) as $receipt |
        if $receipt.task != $task or $receipt.agent != $agent or $done.id != $task or
           $done.closed.by != $agent or $done.closed.commit != $receipt.head
        then error("the accepted receipt and completion do not agree") else . end |
        (($receipt.publication | if type == "object" then .revision // 0 else 0 end)) as $rev |
        if ($rev | type) != "number" or $rev < 0 then error("invalid publication revision") else . end |
        $receipt.head,
        ($receipt + {publication: {at_unix: $now, head: $head, revision: (($rev | floor) + 1)}})
    ') || die "publication record validation failed; claim retained"
accepted_head=${parsed%%$'\n'*}
payload=${parsed#*$'\n'}
# jq for Windows terminates each output line with CRLF.
accepted_head=${accepted_head%$'\r'}
payload=${payload%$'\r'}
[[ "$accepted_head" =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]] || die "the accepted head is not a commit object ID"
git merge-base --is-ancestor "$accepted_head" "$head" 2>/dev/null || die "accepted head is not an ancestor of HEAD"

new_receipt=$(printf '%s' "$payload" | git hash-object -w --stdin) || die "cannot write receipt blob"

push_args=(
    push --atomic
    "--force-with-lease=$claim_ref:$claim_sha"
    "--force-with-lease=$receipt_ref:$receipt_sha"
)
if [ -n "$index_sha" ]; then
    push_args+=("--force-with-lease=$agent_ref:$index_sha")
fi
push_args+=(
    origin
    "$head:refs/heads/$branch"
    ":$claim_ref"
    "$new_receipt:$receipt_ref"
)
if [ -n "$index_sha" ]; then
    push_args+=(":$agent_ref")
fi

if ! git "${push_args[@]}"; then
    exit 1
fi

{
    printf 'start\n'
    printf 'delete %s %s\n' "$claim_ref" "$claim_sha"
    if [ -n "$receipt_sha" ]; then
        printf 'update %s %s %s\n' "$receipt_ref" "$new_receipt" "$receipt_sha"
    else
        printf 'create %s %s\n' "$receipt_ref" "$new_receipt"
    fi
    if [ -n "$index_sha" ]; then
        printf 'delete %s %s\n' "$agent_ref" "$index_sha"
    fi
    printf 'prepare\ncommit\n'
} | git update-ref --no-deref --stdin >/dev/null 2>&1 || true

if [ "$(git rev-parse HEAD)" != "$head" ]; then
    die "published $head, but HEAD changed during publication"
fi
echo "workflow: branch publication and claim release committed atomically" >&2
exit 0
