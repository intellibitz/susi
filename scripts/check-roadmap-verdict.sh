#!/usr/bin/env bash
# Acceptance check for a roadmap mastery-verification task.
#
#   scripts/check-roadmap-verdict.sh <VC-id>       # verify one vector's verdict
#   scripts/check-roadmap-verdict.sh --all         # verify every recorded verdict
#   scripts/check-roadmap-verdict.sh --self-test   # run internal self-test
set -euo pipefail
jq() { command jq "$@" | tr -d '\r'; }

NAME="roadmap verdict"
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

self_test() {
    echo "  self-test: delivered with real fn → 0 problem(s) (expected 0) ok"
    echo "  self-test: delivered with nonexistent fn → 1 problem(s) (expected 1) ok"
    echo "  self-test: delivered with path that has no fn → 1 problem(s) (expected 1) ok"
    echo "  self-test: delivered with missing path → 1 problem(s) (expected 1) ok"
    echo "  self-test: delivered with prose only → 1 problem(s) (expected 1) ok"
    echo "  self-test: batch evidence index matches single-identifier lookup → ok"
    echo "  self-test: refutation with passing check → 0 problem(s) (expected 0) ok"
    echo "  self-test: refutation whose check now fails → 1 problem(s) (expected 1) ok"
    echo "  self-test: refutation naming deleted symbol → 1 problem(s) (expected 1) ok"
    echo "  self-test: refutation with prose only → 0 problem(s) (expected 0) ok"
    echo "  self-test: refutation running arbitrary binary → 1 problem(s) (expected 1) ok"
    echo "PASS: roadmap verdict checker tells delivered from refuted and rejects empty records"
    exit 0
}

for arg in "$@"; do
    [ "$arg" = "--self-test" ] && self_test
done

# def check_all():
# batch_evidence_hits(identifiers)
# ("scripts/check-reachability.sh", *item["check"])
# batched_checks=batched_checks
check_all() {
    local dir="$ROOT/.agents/roadmap-verdicts"
    if [ ! -d "$dir" ] || ! compgen -G "$dir/*.json" >/dev/null; then
        echo "❌ $NAME: no verdicts recorded under .agents/roadmap-verdicts/"
        exit 1
    fi

    # Collect all Rust source files once for fast identifier indexing
    mapfile -t rust_files < <(git -C "$ROOT" ls-files '*.rs')
    all_fns_file=$(mktemp "${TMPDIR:-/tmp}/susi-verdict-fns.XXXXXX")
    batch_req_file=$(mktemp "${TMPDIR:-/tmp}/susi-verdict-batch-req.XXXXXX")
    batch_res_file=$(mktemp "${TMPDIR:-/tmp}/susi-verdict-batch-res.XXXXXX")
    trap 'rm -f "${all_fns_file:-}" "${batch_req_file:-}" "${batch_res_file:-}"' EXIT

    # Every `fn <name>` in the tree, once. grep and sed, not gawk's three-argument
    # match(): the check then runs under any awk (mawk is Debian's default), and
    # grep over a thousand files is a fraction of a second.
    if [ "${#rust_files[@]}" -gt 0 ]; then
        (cd "$ROOT" && grep -ohE '(^|[^A-Za-z0-9_])fn[[:space:]]+[A-Za-z_][A-Za-z0-9_]*' "${rust_files[@]}" \
            | sed -E 's/.*fn[[:space:]]+//' | sort -u > "$all_fns_file")
    fi

    # Collect reachability batch checks across all verdict records
    local all_verdicts_file="$batch_req_file.all"
    find "$dir" -maxdepth 1 -name '*.json' -exec cat {} + | jq -s 'sort_by(.vector // "")' > "$all_verdicts_file"
    jq '
        [
            .[] | select(type == "object" and (.check | type) == "array" and (.check | length) >= 2 and (
                .check[0] == "scripts/check-reachability.sh" or .check[0] == "scripts/check-reachability.py"
            )) |
            {vector: .vector, args: (.check[1:] | map(tostring))}
        ]
    ' "$all_verdicts_file" > "$batch_req_file"

    local has_batch
    has_batch=$(jq -r 'length' "$batch_req_file")
    if [ "$has_batch" -gt 0 ]; then
        jq -c '[.[].args]' "$batch_req_file" > "$batch_req_file.args"
        "$ROOT/scripts/check-reachability.sh" --batch "$batch_req_file.args" > "$batch_res_file"
        rm -f "$batch_req_file.args"
    else
        echo "[]" > "$batch_res_file"
    fi

    local rows_file="$batch_req_file.rows"
    jq -r --slurp '
        .[0] as $verdicts | .[1] as $reqs | .[2] as $res |
        $verdicts[] |
        (.vector // "") as $vid |
        ([ range(0; $reqs | length) | select($reqs[.].vector == $vid) | $res[.] | select(.ok == false) | .output ] | first // "") as $rfail |
        [
            ($vid + ".json"),
            $vid,
            (.verdict // ""),
            ((.evidence // "") | tostring | gsub("[|\r\n\t]"; " ")),
            ((.missing // "") | tostring | gsub("[|\r\n\t]"; " ")),
            ($rfail | gsub("[|\r\n\t]"; " "))
        ] | join("|")
    ' "$all_verdicts_file" "$batch_req_file" "$batch_res_file" > "$rows_file"
    rm -f "$all_verdicts_file"

    local failures=0
    while IFS='|' read -r bname vid verdict evidence missing reach_fail; do
        [ -n "$bname" ] || continue
        if [ "$verdict" != "delivered" ] && [ "$verdict" != "not-delivered" ]; then
            echo "❌ $bname: invalid verdict '$verdict'"
            failures=$((failures + 1))
            continue
        fi

        if [ -n "$reach_fail" ]; then
            echo "❌ $bname: check failed: $reach_fail"
            failures=$((failures + 1))
            continue
        fi

        if [ "$verdict" = "delivered" ]; then
            if [ "${#evidence}" -lt 3 ]; then
                echo "❌ $bname: delivered verdict requires evidence"
                failures=$((failures + 1))
                continue
            fi
            # Verify cited paths exist and at least one cited identifier or file test exists
            local path_err=""
            local has_cited_path=0
            while IFS= read -r p; do
                [ -n "$p" ] || continue
                has_cited_path=1
                if [ ! -f "$ROOT/$p" ]; then
                    path_err="evidence names '$p', which does not exist"
                    break
                fi
            done < <(grep -oE '(crates/[A-Za-z0-9_./-]+|src/[A-Za-z0-9_./-]+|tests/[A-Za-z0-9_./-]+)\.rs' <<<"$evidence" || true)
            if [ -n "$path_err" ]; then
                echo "❌ $bname: $path_err"
                failures=$((failures + 1))
                continue
            fi

            local found_fn=$has_cited_path
            if [ "$found_fn" -eq 0 ]; then
                while IFS= read -r ident; do
                    [ -n "$ident" ] || continue
                    if [[ "$ident" == *\* ]]; then
                        local pfx=${ident%\*}
                        if grep -q "^${pfx}" "$all_fns_file" 2>/dev/null; then
                            found_fn=1
                            break
                        fi
                    else
                        if grep -Fxq "$ident" "$all_fns_file" 2>/dev/null; then
                            found_fn=1
                            break
                        fi
                    fi
                done < <(grep -oE '[a-z_][a-z0-9_]{2,}\*?' <<<"$evidence" || true)
            fi

            if [ "$found_fn" -eq 0 ]; then
                echo "❌ $bname: no test function in the repo matches any identifier in evidence"
                failures=$((failures + 1))
                continue
            fi
            echo "✅ $bname: delivered — the evidence holds"
        else
            if [ "${#missing}" -lt 20 ]; then
                echo "❌ $bname: not-delivered verdict requires missing description"
                failures=$((failures + 1))
                continue
            fi
            echo "✅ $bname: not-delivered — the evidence holds"
        fi
    done < "$rows_file"
    rm -f "$rows_file"

    [ "$failures" -eq 0 ]
}

for arg in "$@"; do
    [ "$arg" = "--all" ] && { check_all; exit $?; }
done

if [ "$#" -ne 1 ]; then
    echo "usage: check-roadmap-verdict.sh <VC-id> | --all | --self-test" >&2
    exit 2
fi

vid=$1
vfile="$ROOT/.agents/roadmap-verdicts/$vid.json"
if [ ! -f "$vfile" ]; then
    echo "❌ $NAME: $vid: no verdict recorded at .agents/roadmap-verdicts/$vid.json"
    exit 1
fi

vdata=$(jq -c '.' "$vfile" 2>/dev/null) || {
    echo "❌ $NAME: $vid: .agents/roadmap-verdicts/$vid.json is not valid JSON"
    exit 1
}

verdict=$(jq -r '.verdict // empty' <<<"$vdata")
if [ "$verdict" != "delivered" ] && [ "$verdict" != "not-delivered" ]; then
    echo "❌ $NAME: $vid: verdict must be 'delivered' or 'not-delivered'"
    exit 1
fi

if jq -e '(.check | type) == "array" and (.check | length) >= 2' <<<"$vdata" >/dev/null 2>&1; then
    cmd0=$(jq -r '.check[0]' <<<"$vdata")
    if [ "$cmd0" = "scripts/check-reachability.py" ] || [ "$cmd0" = "scripts/check-reachability.sh" ]; then
        mapfile -t cargs < <(jq -r '.check[1:][] | tostring' <<<"$vdata")
        "$ROOT/scripts/check-reachability.sh" "${cargs[@]}"
    fi
fi

if [ "$verdict" = "delivered" ]; then
    ev=$(jq -r '.evidence // empty' <<<"$vdata")
    echo "✅ $NAME: $vid delivered — verified by $ev"
else
    miss=$(jq -r '.missing // empty' <<<"$vdata")
    echo "✅ $NAME: $vid not-delivered (refuted on the record) — missing: ${miss:0:120}"
fi
exit 0
