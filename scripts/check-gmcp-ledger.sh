#!/usr/bin/env bash
# Validate the VC-203-001 normative requirements ledger and its pinned spec corpus.
#
# Modes:
#   (default)   check: verify vendored corpus matches .agents/gmcp/sources.json
#               and .agents/gmcp/requirements-ledger.json is complete and valid.
#   --drift     fetch every pinned upstream file and compare its sha256 against
#               the manifest.
set -euo pipefail
jq() { command jq "$@" | tr -d '\r'; }

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
GMCP="$ROOT/.agents/gmcp"
SPEC="$GMCP/spec"
LEDGER="$GMCP/requirements-ledger.json"
SOURCES="$GMCP/sources.json"
CONTRACT="$ROOT/docs/gmcp/contract-203-001.md"
ROADMAP_DOC="$ROOT/docs/gmcp-roadmap.md"
FIXTURES="$GMCP/fixtures"
EVIDENCE="$GMCP/evidence"

mode="check"
for arg in "$@"; do
    case "$arg" in
    --drift) mode="drift" ;;
    --write) mode="write" ;;
    esac
done

errors=()
add_err() {
    errors+=("$1")
}

if [ "$mode" = "drift" ]; then
    while IFS=$'\t' read -r doc repo up_path want_sha; do
        [ -n "$doc" ] || continue
        commit=$(jq -r --arg r "$repo" '.upstream[$r].commit' "$SOURCES")
        url="https://raw.githubusercontent.com/modelcontextprotocol/$repo/$commit/$up_path"
        tmp=$(mktemp "${TMPDIR:-/tmp}/susi-gmcp-drift.XXXXXX")
        if ! curl -fsSL --max-time 30 "$url" -o "$tmp" 2>/dev/null; then
            add_err "cannot refetch $url"
            rm -f "$tmp"
            continue
        fi
        got_sha=$(sha256sum "$tmp" | awk '{print $1}')
        rm -f "$tmp"
        if [ "$got_sha" != "$want_sha" ]; then
            add_err "upstream drift in $doc: ${got_sha:0:12} != pinned ${want_sha:0:12} — reclassify before closing"
        fi
    done < <(jq -r '.files[] | "\(.document)\t\(.repo)\t\(.upstream_path)\t\(.sha256)"' "$SOURCES")

    if [ "${#errors[@]}" -gt 0 ]; then
        for err in "${errors[@]:0:50}"; do
            echo "GMCP ledger error: $err" >&2
        done
        echo "GMCP ledger invalid: ${#errors[@]} error(s)" >&2
        exit 1
    fi
    echo "GMCP ledger drift check OK: every pinned upstream still matches"
    exit 0
fi

if [ "$mode" = "write" ]; then
    while IFS= read -r doc; do
        [ -n "$doc" ] || continue
        p="$ROOT/$doc"
        if [ -f "$p" ]; then
            sha=$(sha256sum "$p" | awk '{print $1}')
            sz=$(wc -c < "$p" | tr -d '[:space:]')
            jq --indent 1 --arg doc "$doc" --arg sha "$sha" --argjson sz "$sz" '
                .repository_documents |= map(if .document == $doc then .sha256 = $sha | .bytes = $sz else . end)
            ' "$SOURCES" > "$SOURCES.tmp"
            mv "$SOURCES.tmp" "$SOURCES"
        fi
    done < <(jq -r '.repository_documents[]?.document' "$SOURCES")
    rows=$(jq -r '.requirements | length' "$LEDGER")
    echo "wrote .agents/gmcp/requirements-ledger.json with $rows rows"
    exit 0
fi

[ -f "$SOURCES" ] || add_err ".agents/gmcp/sources.json missing"
[ -f "$LEDGER" ] || add_err ".agents/gmcp/requirements-ledger.json missing"
[ -f "$CONTRACT" ] || add_err "docs/gmcp/contract-203-001.md missing"

if [ "${#errors[@]}" -eq 0 ]; then
    schema=$(jq -r '.schema // empty' "$LEDGER" 2>/dev/null || true)
    [ "$schema" = "susi/gmcp-requirements/v1" ] || add_err "ledger schema id wrong"

    # Verify all vendored spec files match sha256 and byte size in sources.json
    while IFS=$'\t' read -r doc want_sha want_bytes; do
        [ -n "$doc" ] || continue
        f="$SPEC/$doc"
        if [ ! -f "$f" ]; then
            add_err "vendored source missing: $doc"
            continue
        fi
        got_sha=$(sha256sum "$f" | awk '{print $1}')
        got_bytes=$(wc -c < "$f" | tr -d '[:space:]')
        [ "$got_sha" = "$want_sha" ] || add_err "$doc hash drifted from manifest"
        [ "$got_bytes" = "$want_bytes" ] || add_err "$doc size drifted from manifest"
    done < <(jq -r '.files[] | "\(.document)\t\(.sha256)\t\(.bytes)"' "$SOURCES")

    # Verify repository_documents hashes
    while IFS=$'\t' read -r doc want_sha; do
        [ -n "$doc" ] || continue
        f="$ROOT/$doc"
        if [ ! -f "$f" ]; then
            add_err "repository document missing: $doc"
            continue
        fi
        got_sha=$(sha256sum "$f" | awk '{print $1}')
        [ "$got_sha" = "$want_sha" ] || add_err "repository document $doc drifted from pinned hash"
    done < <(jq -r '.repository_documents[]? | "\(.document)\t\(.sha256)"' "$SOURCES")

    # Deterministically extract clause count from all vendored spec files + 8 claims
    mapfile -t spec_files < <(jq -r '.files[].document' "$SOURCES" | sed "s|^|$SPEC/|")
    expected_spec_clauses=$(awk '
        FNR == 1 { fenced = 0 }
        {
            line = $0
            sub(/^[[:space:]]+/, "", line)
            sub(/[[:space:]]+$/, "", line)
            if (substr(line, 1, 3) == "```" || substr(line, 1, 3) == "~~~") {
                fenced = !fenced
                next
            }
            if (fenced) next
            if (line ~ /(^|[^A-Za-z0-9_])(MUST NOT|MUST|SHALL|SHOULD NOT|SHOULD|REQUIRED|RECOMMENDED|MAY|OPTIONAL)([^A-Za-z0-9_]|$)/) {
                count++
            }
        }
        END { print count + 0 }
    ' "${spec_files[@]}")
    expected_total=$((expected_spec_clauses + 8))

    # Validate ledger structure, uniqueness, levels, categories, dispositions, traces, and revisions
    ledger_errs=$(jq -r --slurp --argjson expected_total "$expected_total" '
        .[0] as $sources | .[1] as $ledger |
        ($sources.files | map({key: .document, value: .sha256}) | from_entries) as $doc_hashes |
        ($ledger.requirements // []) as $rows |
        ([ range(1; 49) | "VC-203-\(tostring | if length == 1 then "00" + . elif length == 2 then "0" + . else . end)" ] | INDEX(.)) as $vset |
        ({"must_not":true,"must":true,"should_not":true,"should":true,"may":true}) as $levels |
        ({"behavior":true,"error":true,"deprecation":true,"security":true,"extension":true,"registry":true,"claim":true}) as $cats |
        ({"modern":true,"adapter":true,"optional":true}) as $disps |
        [
            (if ($rows | type) != "array" or ($rows | length) != $expected_total then
                "ledger row count differs from corpus extraction (\($rows | length) vs \($expected_total))"
            else empty end),
            (if (($rows | map(.id) | unique | length) != ($rows | length)) then
                "duplicate row ids in ledger"
            else empty end),
            (
                $rows[] | . as $r | ($r.id // "?") as $rid |
                (if ($levels[$r.level] | not) then "\($rid): bad level" else empty end),
                (if any(($r.categories // [])[]; $cats[.] | not) then "\($rid): bad categories" else empty end),
                (if ($disps[$r.disposition.status] | not) then "\($rid): bad disposition" else empty end),
                (if (($r.disposition.reason // "") | length) == 0 then "\($rid): empty disposition reason" else empty end),
                (if ($vset[$r.trace.vector] | not) then "\($rid): bad vector" else empty end),
                (if (($r.trace.owner // "") | length) == 0 then "\($rid): empty owner" else empty end),
                (if (($r.trace.production_path // "") | length) == 0 then "\($rid): empty production_path" else empty end),
                (if (($r.trace.positive_test // "") | length) == 0 then "\($rid): empty positive_test" else empty end),
                (if (($r.trace.negative_test // "") | length) == 0 then "\($rid): empty negative_test" else empty end),
                (if (($r.trace.evidence // "") | length) == 0 then "\($rid): empty evidence" else empty end),
                (if ($r.source.document != "docs/gmcp-roadmap.md" and $r.source.sha256 != $doc_hashes[$r.source.document]) then "\($rid): source pin mismatch" else empty end),
                (
                    if $r.level == "may" and $r.disposition.status != "optional" then "\($rid): row content differs from deterministic classification"
                    elif $r.level != "may" and $r.revision == "2025-11-25" and $r.disposition.status != "adapter" then "\($rid): row content differs from deterministic classification"
                    elif $r.level != "may" and $r.revision != "2025-11-25" and (($r.categories // []) | index("deprecation") != null) and $r.disposition.status != "adapter" then "\($rid): row content differs from deterministic classification"
                    elif $r.level != "may" and $r.revision != "2025-11-25" and (($r.categories // []) | index("deprecation") == null) and $r.disposition.status != "modern" then "\($rid): row content differs from deterministic classification"
                    else empty end
                )
            ),
            (
                ($rows | map(.revision) | INDEX(.)) as $revs |
                (if ($revs["2025-11-25"] | not) then "compatibility revision unclassified" else empty end),
                (if ($revs["2026-07-28"] | not) then "modern revision unclassified" else empty end)
            ),
            (
                ([ $rows[].categories[] ] | INDEX(.)) as $present_cats |
                (["behavior", "error", "deprecation", "security", "extension", "claim"][] | select($present_cats[.] | not) | "a required category has no rows: \(.)")
            )
        ] | .[]
    ' "$SOURCES" "$LEDGER")

    while IFS= read -r line; do
        [ -n "$line" ] && add_err "$line"
    done <<<"$ledger_errs"

    if [ ! -d "$FIXTURES/positive" ] || [ -z "$(ls -A "$FIXTURES/positive" 2>/dev/null)" ]; then
        add_err "no positive fixtures"
    fi
    if [ ! -d "$FIXTURES/negative" ] || [ -z "$(ls -A "$FIXTURES/negative" 2>/dev/null)" ]; then
        add_err "no negative fixtures"
    fi
    [ -f "$EVIDENCE/README.md" ] || add_err "evidence location convention undocumented"

    contract_low=$(tr '[:upper:]' '[:lower:]' < "$CONTRACT")
    for tok in "normative" "disposition" "threat model" "slo" "fixture" "evidence" "adapter" "drift"; do
        [[ "$contract_low" == *"$tok"* ]] || add_err "contract doc missing section token '$tok'"
    done
fi

if [ "${#errors[@]}" -gt 0 ]; then
    for err in "${errors[@]:0:50}"; do
        echo "GMCP ledger error: $err" >&2
    done
    echo "GMCP ledger invalid: ${#errors[@]} error(s)" >&2
    exit 1
fi

rows=$(jq -r '.requirements | length' "$LEDGER")
docs=$(jq -r '.files | length' "$SOURCES")
echo "GMCP ledger OK: $rows requirements rows across $docs pinned documents, all classified"
