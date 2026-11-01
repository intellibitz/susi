#!/usr/bin/env bash
# Check the RSI proof scorecard (.agents/rsi/scorecard.json) against repo reality.
set -euo pipefail
jq() { command jq "$@" | tr -d '\r'; }

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SCORECARD="$ROOT/.agents/rsi/scorecard.json"

if [ ! -f "$SCORECARD" ]; then
    echo "missing $SCORECARD" >&2
    exit 1
fi

jq -e '
    def is_nonempty_str: type == "string" and length > 0;
    def is_str_list: type == "array" and length > 0 and all(.[]; is_nonempty_str);
    if .schema != 1 then error("scorecard schema must be 1")
    elif (.program | is_nonempty_str | not) then error("program must be a non-empty string")
    elif (.updated | is_nonempty_str | not) then error("updated must be a non-empty string")
    elif (.non_negotiable_rules | is_str_list | not) then error("non_negotiable_rules must be a non-empty string list")
    elif (.dimensions | type) != "array" or (.dimensions | length) < 7 then error("dimensions must contain at least 7 entries")
    else true
    end
' "$SCORECARD" >/dev/null

errors=()
seen_ids=" "
total_weight=0

num_dims=$(jq -r '.dimensions | length' "$SCORECARD")
for (( i = 0; i < num_dims; i++ )); do
    dim=$(jq -c ".dimensions[$i]" "$SCORECARD")
    did=$(jq -r '.id // empty' <<<"$dim")
    if [ -z "$did" ]; then
        errors+=("dimension has invalid id")
        continue
    fi
    case "$seen_ids" in
    *" $did "*) errors+=("duplicate dimension id: $did") ;;
    esac
    seen_ids="$seen_ids$did "

    title=$(jq -r '.title // empty' <<<"$dim")
    [ -n "$title" ] || errors+=("$did: missing title")

    weight=$(jq -r '.weight // 0' <<<"$dim")
    if [[ ! "$weight" =~ ^[0-9]+$ ]] || [ "$weight" -le 0 ]; then
        errors+=("$did: invalid weight $weight")
    else
        total_weight=$((total_weight + weight))
    fi

    score=$(jq -r '.current_score // -1' <<<"$dim")
    target=$(jq -r '.target_score // -1' <<<"$dim")
    if [[ ! "$score" =~ ^[0-9]+$ ]] || [ "$score" -lt 0 ] || [ "$score" -gt 100 ]; then
        errors+=("$did: invalid current_score $score")
    fi
    if [ "$target" != "100" ]; then
        errors+=("$did: target_score must be 100, got $target")
    fi

    for list_field in current_controls remaining_gaps required_proofs; do
        if ! jq -e ".$list_field | type == \"array\" and length > 0 and all(.[]; type == \"string\" and length > 0)" <<<"$dim" >/dev/null 2>&1; then
            errors+=("$did: $list_field must be a non-empty list of strings")
        fi
    done

    if ! jq -e '.evidence_paths | type == "array" and length > 0' <<<"$dim" >/dev/null 2>&1; then
        errors+=("$did: evidence_paths must be a non-empty list of strings")
    else
        while IFS= read -r rel; do
            [ -n "$rel" ] || continue
            if [[ "$rel" == /* ]] || [[ "$rel" == *".."* ]]; then
                errors+=("$did: evidence path must be repo-relative: $rel")
            elif [ ! -e "$ROOT/$rel" ]; then
                errors+=("$did: missing evidence path $rel")
            fi
        done < <(jq -r '.evidence_paths[]' <<<"$dim")
    fi

    if ! jq -e '.verification_commands | type == "array" and length > 0' <<<"$dim" >/dev/null 2>&1; then
        errors+=("$did: verification_commands must be a non-empty list")
    else
        while IFS= read -r cmd0; do
            case "$cmd0" in
            cargo | susi | scripts/*)
                if [[ "$cmd0" == scripts/* ]] && [ ! -e "$ROOT/$cmd0" ]; then
                    errors+=("$did: missing verification script $cmd0")
                fi
                ;;
            *)
                errors+=("$did: unsupported verification command executable '$cmd0'")
                ;;
            esac
        done < <(jq -r '.verification_commands[] | if (type == "array" and length > 0) then .[0] else "__bad__" end' <<<"$dim")
    fi
done

if [ "$total_weight" -ne 100 ]; then
    errors+=("dimension weights must sum to 100, got $total_weight")
fi

if [ "${#errors[@]}" -gt 0 ]; then
    for err in "${errors[@]}"; do
        echo "❌ rsi scorecard: $err" >&2
    done
    exit 1
fi

overall=$(jq -r '([.dimensions[] | (.current_score * .weight)] | add) / 100.0' "$SCORECARD")
lowest=$(jq -r '.dimensions | min_by(.current_score) | "\(.id)=\(.current_score)"' "$SCORECARD")
printf '✅ rsi scorecard: %d dimensions valid, weighted score %.1f/100, lowest %s\n' "$num_dims" "$overall" "$lowest"
