#!/usr/bin/env bash
# Validate the VC-203 GMCP completion program and its task dependency proof.
set -euo pipefail
jq() { command jq "$@" | tr -d '\r'; }

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ROADMAP="$ROOT/.agents/roadmap.json"
DOC="$ROOT/docs/gmcp-roadmap.md"
README="$ROOT/README.md"

errors=()
require_cond() {
    local cond=$1 msg=$2
    if [ "$cond" != "true" ]; then
        errors+=("$msg")
    fi
}

if [ ! -f "$ROADMAP" ]; then
    echo "GMCP roadmap error: .agents/roadmap.json missing" >&2
    exit 1
fi

open_tasks="[]"
if [ -d "$ROOT/.agents/tasks" ] && compgen -G "$ROOT/.agents/tasks/*.json" >/dev/null; then
    open_tasks=$(find "$ROOT/.agents/tasks" -maxdepth 1 -name '*.json' -exec cat {} + | jq -s 'map(. + {_done: false})')
fi
done_tasks="[]"
if [ -d "$ROOT/.agents/tasks/done" ] && compgen -G "$ROOT/.agents/tasks/done/*.json" >/dev/null; then
    done_tasks=$(find "$ROOT/.agents/tasks/done" -maxdepth 1 -name '*.json' -exec cat {} + | jq -s 'map(. + {_done: true})')
fi

verdicts_list="[]"
if [ -d "$ROOT/.agents/roadmap-verdicts" ] && compgen -G "$ROOT/.agents/roadmap-verdicts/*.json" >/dev/null; then
    verdicts_list=$( (cd "$ROOT/.agents/roadmap-verdicts" && printf '%s\n' *.json | sed 's/\.json$//' | jq -R . | jq -s .) )
fi

jq_errors=$( { cat "$ROADMAP"; printf '\n%s\n%s\n%s\n' "$open_tasks" "$done_tasks" "$verdicts_list"; } | jq -r --slurp '
    .[0].vectors as $vectors |
    (.[1] + .[2]) as $all_tasks |
    (.[3] | INDEX(.)) as $verdicts |
    ([ range(1; 49) | "VC-203-\(tostring | if length == 1 then "00" + . elif length == 2 then "0" + . else . end)" ]) as $vids |
    ($vids | INDEX(.)) as $vset |
    ($vectors | map(select(type == "object" and (.id | type) == "string") | {key: .id, value: .}) | from_entries) as $by_id |
    ($all_tasks | map(select(type == "object" and (.id | type) == "string") | {key: .id, value: .}) | from_entries) as $tasks_by_id |
    [
        (if ($vectors | type) != "array" then ".agents/roadmap.json vectors must be a list" else empty end),
        ($vids[] | select($by_id[.] == null) | "missing VC-203 vector: \(.)"),
        (
            $vids[] | . as $vid | ($by_id[$vid] // null) as $v |
            if $v == null then empty
            else
                (if ($v.depends_on | type) != "array" then "\($vid) depends_on must be a list" else empty end),
                (if (($v.depends_on // []) | index($vid)) != null then "\($vid) depends on itself" else empty end),
                (($v.depends_on // [])[] | select($by_id[.] == null) | "\($vid) has unknown dependency \(.)"),
                (if ($v.priority != "P0" and $v.priority != "P1" and $v.priority != "P2") then "\($vid) has invalid priority" else empty end),
                (if (($v.type // "") | length) == 0 then "\($vid) has no type" else empty end),
                (if (($v.vector // "") | length) == 0 then "\($vid) has no capability name" else empty end),
                (if (($v.mastery_target // "") | length) < 160 then "\($vid) mastery target is not sufficiently explicit" else empty end),
                (if (((($v.progress // "") | startswith("NOT DELIVERED")) or (($v.progress // "") | startswith("DELIVERED"))) | not) then "\($vid) progress must explicitly say NOT DELIVERED or DELIVERED" else empty end)
            end
        ),
        (
            (($by_id["VC-203-048"].depends_on // []) | unique | sort) as $deps48 |
            (($vids[0:47]) | sort) as $want48 |
            if $deps48 != $want48 then "VC-203-048 must depend directly on every preceding VC-203 vector" else empty end
        ),
        (
            [
                $all_tasks[] |
                (.goal // "") as $g |
                [ $g | scan("Requirement (GMCP-203-([0-9]{3})-(01|02|03|04|05|06|M))") ] as $m |
                if ($m | length) == 1 then
                    {task_id: .id, req_id: $m[0][0], num: $m[0][1], suffix: $m[0][2], roadmap: .roadmap, done: ._done, cmd: (.accept.cmd // []), size: .size, deps: (.deps // [])}
                else empty end
            ] as $gmcp_tasks |
            (if ($gmcp_tasks | length) != 336 then "expected 336 unique requirement tasks, found \($gmcp_tasks | length)" else empty end)
        )
    ] | .[]
')

while IFS= read -r line; do
    [ -n "$line" ] && errors+=("$line")
done <<<"$jq_errors"

if [ ! -f "$DOC" ]; then
    errors+=("docs/gmcp-roadmap.md is missing")
else
    doc_text=$(cat "$DOC")
    for n in $(seq -w 1 48); do
        vid="VC-203-0$n"
        [[ "$doc_text" == *"$vid"* ]] || errors+=("docs/gmcp-roadmap.md does not mention $vid")
    done
    for tok in "MCP 2026-07-28" "336 open implementation" "gmcp_complete_mastery" "Official conformance" "Standards and metric drift loop" "zero unresolved critical/high"; do
        [[ "$doc_text" == *"$tok"* ]] || errors+=("docs/gmcp-roadmap.md lacks required completion token '$tok'")
    done
fi

if [ ! -f "$README" ] || ! grep -Fq "docs/gmcp-roadmap.md" "$README"; then
    errors+=("README.md does not link the GMCP completion roadmap")
fi

if [ "${#errors[@]}" -gt 0 ]; then
    for err in "${errors[@]}"; do
        echo "GMCP roadmap error: $err" >&2
    done
    echo "GMCP roadmap invalid: ${#errors[@]} error(s)" >&2
    exit 1
fi

echo "GMCP roadmap OK: 48 vectors, 336 requirement tasks, acyclic dependencies, complete mastery fan-in"
