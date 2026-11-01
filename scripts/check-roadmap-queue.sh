#!/usr/bin/env bash
# Queue the work the roadmap still owes, and prove the queue is not dry.
#
#   scripts/check-roadmap-queue.sh                    # report; exit 1 when unqueued
#   scripts/check-roadmap-queue.sh --consistency-only # verify narrative vs verdicts
#   scripts/check-roadmap-queue.sh --queue            # file missing verification tasks
#   scripts/check-roadmap-queue.sh --self-test        # run internal consistency self-test
set -euo pipefail
jq() { command jq "$@" | tr -d '\r'; }

NAME="roadmap queue"
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

check_consistency_json() {
    local roadmap_json=$1 verdicts_json=$2
    printf '%s\n%s\n' "$roadmap_json" "$verdicts_json" | jq -r --slurp '
        .[0] as $vectors | .[1] as $verdicts |
        $vectors[] | . as $v |
        ($verdicts[$v.id] // null) as $rec |
        if $rec == null then empty
        else
            (($v.progress // "") | gsub("^[[:space:]]+|[[:space:]]+$"; "")) as $prog |
            ($prog | startswith("DELIVERED")) as $claims |
            if $rec.verdict == "delivered" and ($claims | not) then
                "\($v.id) has a delivered verdict on record, but its narrative does not claim delivery: \($prog[0:80])"
            elif $rec.verdict == "not-delivered" and $claims then
                "\($v.id) is refuted on record, but its narrative still claims delivery: \($prog[0:80])"
            else empty
            end
        end
    '
}

self_test() {
    local failures=0
    run_case() {
        local label=$1 progress=$2 verdict=$3 expected=$4 got ok
        local probs
        probs=$(check_consistency_json "[{\"id\":\"VC-0-1\",\"progress\":\"$progress\"}]" "{\"VC-0-1\":{\"verdict\":\"$verdict\"}}")
        if [ -n "$probs" ]; then got=1; else got=0; fi
        if [ "$got" -eq "$expected" ]; then
            echo "  self-test: $label → $got problem(s) (expected $expected) ok"
        else
            echo "  self-test: $label → $got problem(s) (expected $expected) WRONG"
            failures=$((failures + 1))
        fi
    }
    run_case "delivered, narrative agrees" "DELIVERED: proof" "delivered" 0
    run_case "delivered, narrative disagrees" "PARTIAL: half" "delivered" 1
    run_case "refuted, narrative agrees" "PARTIAL: half" "not-delivered" 0
    run_case "refuted, narrative disagrees" "DELIVERED: claimed" "not-delivered" 1
    local extra
    extra=$(check_consistency_json '[{"id":"VC-0-1","progress":"PARTIAL"}]' '{}')
    if [ -n "$extra" ]; then
        failures=$((failures + 1))
        echo "  self-test: no verdict recorded → WRONG (must not be a contradiction)"
    fi
    if [ "$failures" -ne 0 ]; then
        echo "self-test FAILED: $failures case(s) wrong"
        exit 1
    fi
    echo "PASS: a narrative that contradicts its recorded verdict is a failure"
    exit 0
}

for arg in "$@"; do
    [ "$arg" = "--self-test" ] && self_test
done

load_verdicts_json() {
    if [ ! -d ".agents/roadmap-verdicts" ] || ! compgen -G ".agents/roadmap-verdicts/*.json" >/dev/null; then
        echo "{}"
        return
    fi
    find .agents/roadmap-verdicts -maxdepth 1 -name '*.json' -exec cat {} + | jq -s '
        map(select(type == "object" and (.verdict == "delivered" or .verdict == "not-delivered") and (.vector | type) == "string") | {key: .vector, value: .})
        | from_entries
    ' 2>/dev/null || echo "{}"
}

vectors_json=$(jq -c '.vectors' .agents/roadmap.json)
verdicts_json=$(load_verdicts_json)
problems=$(check_consistency_json "$vectors_json" "$verdicts_json")

for arg in "$@"; do
    if [ "$arg" = "--consistency-only" ]; then
        if [ -n "$problems" ]; then
            while IFS= read -r p; do
                [ -n "$p" ] && echo "❌ $NAME: $p"
            done <<<"$problems"
            exit 1
        fi
        echo "✅ $NAME: every recorded verdict agrees with its vector's narrative"
        exit 0
    fi
done

if [ -n "$problems" ]; then
    count=0
    while IFS= read -r p; do
        if [ -n "$p" ]; then
            echo "❌ $NAME: $p"
            count=$((count + 1))
        fi
    done <<<"$problems"
    printf '\n%d vector(s) contradict their recorded verdict; fix the narrative (the verdict is the evidence, the narrative is the claim)\n' "$count"
    exit 1
fi

queue=0
for arg in "$@"; do
    [ "$arg" = "--queue" ] && queue=1
done

open_tasks_json="[]"
if [ -d ".agents/tasks" ] && compgen -G ".agents/tasks/*.json" >/dev/null; then
    open_tasks_json=$(find .agents/tasks -maxdepth 1 -name '*.json' -exec cat {} + | jq -s '.' 2>/dev/null || echo "[]")
fi
done_tasks_json="[]"
if [ -d ".agents/tasks/done" ] && compgen -G ".agents/tasks/done/*.json" >/dev/null; then
    done_tasks_json=$(find .agents/tasks/done -maxdepth 1 -name '*.json' -exec cat {} + | jq -s '.' 2>/dev/null || echo "[]")
fi

summary=$(printf '%s\n%s\n%s\n%s\n' "$vectors_json" "$open_tasks_json" "$done_tasks_json" "$verdicts_json" | jq -r --slurp '
    .[0] as $roadmap | .[1] as $open | .[2] as $done | .[3] as $verdicts |
    ($open | map(.roadmap // empty) | INDEX(.)) as $linked_open |
    (($open + $done) | map(.roadmap // empty) | INDEX(.)) as $linked_any |
    ($roadmap | map(select($linked_any[.id] != null))) as $covered |
    ($roadmap | map(select(((.progress // "") | gsub("^[[:space:]]+|[[:space:]]+$"; "")) | startswith("DELIVERED")))) as $claimed |
    ($roadmap | map(select((((.progress // "") | gsub("^[[:space:]]+|[[:space:]]+$"; "")) | startswith("DELIVERED")) | not))) as $unverified |
    ($unverified | map(select($linked_open[.id] == null and $verdicts[.id] == null))) as $unqueued |
    {
        total: ($roadmap | length),
        covered: ($covered | length),
        claimed: ($claimed | length),
        unverified: ($unverified | length),
        recorded: ($verdicts | length),
        unqueued_count: ($unqueued | length),
        unqueued: ($unqueued | sort_by(.priority, .id))
    }
')

total=$(jq -r '.total' <<<"$summary")
covered=$(jq -r '.covered' <<<"$summary")
claimed=$(jq -r '.claimed' <<<"$summary")
unverified=$(jq -r '.unverified' <<<"$summary")
recorded=$(jq -r '.recorded' <<<"$summary")
unqueued_count=$(jq -r '.unqueued_count' <<<"$summary")

echo "$NAME: $total vectors — coverage $covered linked to a task, mastery $claimed claim delivery, $unverified do not ($recorded verified or refuted on record, $unqueued_count unqueued)"

if [ "$unqueued_count" -eq 0 ]; then
    echo "✅ $NAME: every vector claims delivery, is verified or refuted on record, or has work in the queue"
    exit 0
fi

if [ "$queue" -eq 0 ]; then
    jq -r --arg name "$NAME" '
        .unqueued[] |
        (.id | ascii_downcase | gsub("-"; "_")) as $fname |
        "❌ \($name): \(.id) (\(.priority)) \(.vector) — not delivered by its own narrative and nothing queued: susi tasks add \"Verify mastery: \(.vector)\" --accept \"cargo nextest run --locked -E test(\($fname)_mastery)\" --roadmap \(.id)"
    ' <<<"$summary"
    printf '\n%d vector(s) would be left with no work once the queue empties; re-run with --queue to file them\n' "$unqueued_count"
    exit 1
fi

filed=0
while IFS= read -r vjson; do
    [ -n "$vjson" ] || continue
    vid=$(jq -r '.id' <<<"$vjson")
    vprio=$(jq -r '.priority' <<<"$vjson")
    vname=$(jq -r '.vector' <<<"$vjson")
    fname=$(printf '%s' "$vid" | tr '[:upper:]-' '[:lower:]_')
    accept="cargo nextest run --locked -E test(${fname}_mastery)"
    goal=$(jq -r --arg fname "$fname" '
        "\(.id) (\(.priority)) claims mastery of: \(.mastery_target // ""). Its own narrative does not claim delivery: \((.progress // "") | gsub("^[[:space:]]+|[[:space:]]+$"; "") | .[0:400]). Prove or refute it: add a nonzero test named \($fname)_mastery_* that exercises the capability on its production path, and record exactly which part is missing when it cannot be proven. Coverage is not mastery: this closes only when the capability is verified, and a linked closed task is not evidence."
    ' <<<"$vjson")
    size="m"
    [ "$vprio" = "P0" ] && size="l"
    if out=$(susi tasks add "Verify mastery: $vname" --goal "$goal" --accept "$accept" --size "$size" --roadmap "$vid" --agent "DEEPSEEK" 2>&1); then
        filed=$((filed + 1))
        last_line=$(printf '%s\n' "$out" | tail -n 1)
        echo "  filed $last_line"
    else
        last_line=$(printf '%s\n' "$out" | tail -n 1)
        echo "❌ $NAME: could not file $vid: $last_line"
    fi
done < <(jq -c '.unqueued[]' <<<"$summary")

echo "filed $filed verification task(s); re-run without --queue to confirm"
[ "$filed" -eq "$unqueued_count" ]
