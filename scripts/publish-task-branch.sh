#!/usr/bin/env bash
# Publish accepted code and release ownership in one compare-and-swap push.
#
# Exit 75 requests the existing separate queue path (cloud relay, another queue
# remote, or an already released claim). Other failures never fall back or release.
set -euo pipefail
jq() { command jq "$@" | tr -d '\r'; }

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
jq -e 'type == "object"' <<<"$claim_body" >/dev/null 2>&1 || die "invalid workflow record: $claim_ref"

index_sha=$(git rev-parse --verify --quiet "$agent_ref" 2>/dev/null || true)
index_body=""
if [ -n "$index_sha" ]; then
    index_body=$(git cat-file -p "$index_sha" 2>/dev/null) || die "invalid workflow blob: $agent_ref"
    jq -e 'type == "object"' <<<"$index_body" >/dev/null 2>&1 || die "invalid workflow record: $agent_ref"
fi

receipt_sha=$(git rev-parse --verify --quiet "$receipt_ref" 2>/dev/null || true)
done_sha=$(git rev-parse --verify --quiet "$done_ref" 2>/dev/null || true)
if [ -z "$receipt_sha" ] || [ -z "$done_sha" ]; then
    die "publication requires the accepted receipt and committed completion"
fi
receipt_body=$(git cat-file -p "$receipt_sha" 2>/dev/null) || die "invalid workflow blob: $receipt_ref"
done_body=$(git cat-file -p "$done_sha" 2>/dev/null) || die "invalid workflow blob: $done_ref"

for rec in "$claim_body" "$index_body"; do
    [ -n "$rec" ] || continue
    jq -e --arg task "$task" --arg agent "$agent" --arg branch "$branch" \
        '.task == $task and .agent == $agent and (.branch == null or .branch == $branch)' \
        <<<"$rec" >/dev/null 2>&1 || die "ownership changed before publication; claim retained"
done

jq -e '.closed | type == "object"' <<<"$done_body" >/dev/null 2>&1 || die "the committed completion has no closure record"
printf '%s\n%s\n' "$receipt_body" "$done_body" | jq -e --slurp --arg task "$task" --arg agent "$agent" '
    .[0] as $acc | .[1] as $d |
    $acc.task == $task and $acc.agent == $agent and $d.id == $task and $d.closed.by == $agent and $d.closed.commit == $acc.head
' >/dev/null 2>&1 || die "the accepted receipt and completion do not agree"

accepted_head=$(jq -r '.head // empty' <<<"$receipt_body")
[[ "$accepted_head" =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]] || die "the accepted head is not a commit object ID"
git merge-base --is-ancestor "$accepted_head" "$head" 2>/dev/null || die "accepted head is not an ancestor of HEAD"

now=$(date +%s)
payload=$(jq -c -S --arg head "$head" --argjson now "$now" '
    ((.publication | if type == "object" then .revision // 0 else 0 end)) as $rev |
    if ($rev | type) != "number" or $rev < 0 then error("invalid publication revision")
    else
        .publication = {"at_unix": $now, "head": $head, "revision": (($rev | floor) + 1)}
    end
' <<<"$receipt_body") || die "invalid publication revision"

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
    printf 'update %s %s %s\n' "$receipt_ref" "$new_receipt" "$receipt_sha"
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
