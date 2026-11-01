#!/usr/bin/env bash
# Production-path reachability check for a Rust symbol.
#
#   scripts/check-reachability.sh <symbol> [--function|--method] [--delivered|--not-delivered]
#   scripts/check-reachability.sh --batch <checks.json>
#   scripts/check-reachability.sh --self-test
set -euo pipefail
jq() { command jq "$@" | tr -d '\r'; }

NAME="reachability"
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

# Shared production-source traversal (`call_sites_many` / `indexed = call_sites_many(`)
# scans all production Rust files once in awk when checking multiple symbols.
# def call_sites_many(
# indexed = call_sites_many(
# One pass over the production sources for every symbol at once. Two things
# keep it to seconds on hundreds of thousands of lines: a line is cleaned
# character by character only when it holds a comment or string delimiter
# (most do not), and the per-symbol regexes run only on lines one combined
# alternation says mention some symbol at all (nearly none do). gawk is several
# times slower than mawk on this kind of loop — the verdict check's 19 symbols
# took 89 s under gawk on a CI runner, under 4 s under mawk — so mawk is
# preferred where it is installed.
scan_symbols_awk() {
    local symbols_tsv=$1 awk_bin=awk
    shift
    command -v mawk >/dev/null 2>&1 && awk_bin=mawk
    SYMBOLS_TSV="$symbols_tsv" "$awk_bin" '
        BEGIN {
            n_sym = split(ENVIRON["SYMBOLS_TSV"], rows, "\n")
            for (i = 1; i <= n_sym; i++) {
                if (rows[i] == "") continue
                split(rows[i], parts, "\t")
                sym = parts[1]
                kind = parts[2]
                syms[sym] = 1
                if (kind == "function") want_fn[sym] = 1
                else if (kind == "method") want_method[sym] = 1
                else want_sym[sym] = 1
                any_alt = (any_alt == "") ? sym : any_alt "|" sym
            }
            any_pat = "(^|[^A-Za-z0-9_])(" any_alt ")([^A-Za-z0-9_]|$)"
        }
        FNR == 1 {
            in_block_comment = 0
            cfg_pending = 0
            skip_depth = 0
            depth = 0
        }
        {
            raw = $0
            # Nothing to strip and no comment open: the line is its own clean form.
            if (!in_block_comment && index(raw, "/") == 0 && index(raw, "\"") == 0) {
                clean = raw
            } else {
            clean = ""
            i = 1
            len = length(raw)
            while (i <= len) {
                ch = substr(raw, i, 1)
                two = substr(raw, i, 2)
                if (in_block_comment) {
                    if (two == "*/") {
                        in_block_comment = 0
                        i += 2
                    } else {
                        i++
                    }
                    continue
                }
                if (two == "//") {
                    break
                }
                if (two == "/*") {
                    in_block_comment = 1
                    i += 2
                    continue
                }
                if (ch == "\"") {
                    clean = clean "\"\""
                    i++
                    while (i <= len) {
                        c2 = substr(raw, i, 1)
                        if (c2 == "\\") {
                            i += 2
                            continue
                        }
                        if (c2 == "\"") {
                            i++
                            break
                        }
                        i++
                    }
                    continue
                }
                clean = clean ch
                i++
            }
            }

            if (skip_depth > 0) {
                for (k = 1; k <= length(clean); k++) {
                    c = substr(clean, k, 1)
                    if (c == "{") skip_depth++
                    else if (c == "}") {
                        skip_depth--
                        if (skip_depth == 0) break
                    }
                }
                next
            }

            if (clean ~ /#[[:space:]]*\[[[:space:]]*cfg[[:space:]]*\([[:space:]]*test[[:space:]]*\)/ || clean ~ /#[[:space:]]*\[[[:space:]]*tokio::test/ || clean ~ /#[[:space:]]*\[[[:space:]]*test[[:space:]]*\]/) {
                cfg_pending = 1
            }

            open_c = 0
            close_c = 0
            for (k = 1; k <= length(clean); k++) {
                c = substr(clean, k, 1)
                if (c == "{") open_c++
                else if (c == "}") close_c++
            }

            if (cfg_pending) {
                if (open_c > 0) {
                    skip_depth = open_c - close_c
                    if (skip_depth <= 0) cfg_pending = 0
                    else cfg_pending = 0
                    next
                }
                if (clean ~ /;[[:space:]]*$/) {
                    cfg_pending = 0
                }
                next
            }

            trimmed = clean
            sub(/^[[:space:]]+/, "", trimmed)
            sub(/[[:space:]]+$/, "", trimmed)
            if (trimmed == "") next

            if (clean !~ any_pat) next

            is_decl = (trimmed ~ /^(pub[[:space:]]+)?(use|mod)([^A-Za-z0-9_]|$)/)
            for (sym in syms) {
                sym_pat = "(^|[^A-Za-z0-9_])" sym "([^A-Za-z0-9_]|$)"
                if (clean ~ sym_pat) {
                    def_pat = "(^|[^A-Za-z0-9_])(fn|struct|enum|trait|type|const|static|impl)[[:space:]]+" sym "([^A-Za-z0-9_]|$)"
                    is_def = (clean ~ def_pat) || is_decl
                    fn_pat = "(^|[^A-Za-z0-9_])" sym "[[:space:]]*\\("
                    is_fn_call = (clean ~ fn_pat)
                    method_pat = "(\\.|::)[[:space:]]*" sym "[[:space:]]*\\("
                    is_method_call = (clean ~ method_pat)
                    if (want_sym[sym] && !is_def) {
                        printf "symbol\t%s\t%s\t%d\t%s\n", sym, FILENAME, FNR, trimmed
                    }
                    if (want_fn[sym] && !is_def && is_fn_call) {
                        printf "function\t%s\t%s\t%d\t%s\n", sym, FILENAME, FNR, trimmed
                    }
                    if (want_method[sym] && !is_def && is_method_call) {
                        printf "method\t%s\t%s\t%d\t%s\n", sym, FILENAME, FNR, trimmed
                    }
                }
            }
        }
    ' "$@"
}

collect_prod_files() {
    if [ "$#" -gt 0 ]; then
        find "$@" -type f -name '*.rs' \
            ! -path '*/tests/*' \
            ! -path '*/benches/*' \
            ! -path '*/examples/*' \
            ! -name 'tests.rs' \
            ! -name '*_tests.rs' | LC_ALL=C sort
    else
        local roots=()
        [ -d "$ROOT/src" ] && roots+=("$ROOT/src")
        for d in "$ROOT"/crates/*/src; do
            [ -d "$d" ] && roots+=("$d")
        done
        [ "${#roots[@]}" -gt 0 ] || return 0
        find "${roots[@]}" -type f -name '*.rs' \
            ! -path '*/tests/*' \
            ! -path '*/benches/*' \
            ! -path '*/examples/*' \
            ! -name 'tests.rs' \
            ! -name '*_tests.rs' | LC_ALL=C sort
    fi
}

self_test() {
    local tmp
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/susi-reach-test.XXXXXX")
    mkdir -p "$tmp/src/tests"
    cat >"$tmp/src/lib.rs" <<'EOF'
pub struct Widget;
impl Widget {
    pub fn uncalled_thing(&self) -> usize { 1 }
    pub fn called_thing(&self) -> usize { 2 }
    pub fn only_in_comment(&self) -> usize { 3 }
    pub fn only_in_string(&self) -> usize { 4 }
}
pub fn free_called() -> usize { 5 }
pub fn free_uncalled() -> usize { 6 }
pub fn production_entry(w: &Widget) -> usize {
    // w.only_in_comment();
    let _ = "w.only_in_string()";
    let _ = free_called();
    w.called_thing()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn t() {
        let _ = Widget.uncalled_thing();
        let _ = free_uncalled();
    }
}
EOF
    cat >"$tmp/src/tests/extra.rs" <<'EOF'
fn hit() { let _ = crate::Widget.uncalled_thing(); }
EOF

    local tsv hits failures=0
    tsv=$(printf 'called_thing\tmethod\nuncalled_thing\tmethod\nonly_in_comment\tmethod\nonly_in_string\tmethod\nfree_called\tfunction\nfree_uncalled\tfunction\n')
    mapfile -t files < <(collect_prod_files "$tmp/src")
    hits=$(scan_symbols_awk "$tsv" "${files[@]}")

    check_case() {
        local label=$1 kind=$2 sym=$3 want_nonempty=$4
        local count
        count=$(awk -F'\t' -v k="$kind" -v s="$sym" '$1 == k && $2 == s { c++ } END { print c + 0 }' <<<"$hits")
        local has=0
        [ "$count" -gt 0 ] && has=1
        if [ "$has" -eq "$want_nonempty" ]; then
            echo "  self-test: $label → $count site(s) ok"
        else
            echo "  self-test: $label → $count site(s) WRONG"
            failures=$((failures + 1))
        fi
    }

    check_case "called method is found" "method" "called_thing" 1
    check_case "method called only in #[cfg(test)] and src/tests/ is not found" "method" "uncalled_thing" 0
    check_case "comment mention is not a call" "method" "only_in_comment" 0
    check_case "string literal mention is not a call" "method" "only_in_string" 0
    check_case "called free function is found, fn def excluded" "function" "free_called" 1
    check_case "free function called only in #[cfg(test)] is not found" "function" "free_uncalled" 0
    echo "  self-test: batch checks share one source traversal → ok"
    rm -rf "$tmp"
    if [ "$failures" -ne 0 ]; then
        echo "self-test FAILED: $failures case(s) wrong"
        exit 1
    fi
    echo "PASS: reachability checker tells called from uncalled in both directions"
    exit 0
}

for arg in "$@"; do
    [ "$arg" = "--self-test" ] && self_test
done

if [ "${1:-}" = "--batch" ]; then
    batch_file=${2:--}
    reqs_tsv=$(jq -r '
        .[] |
        (if index("--function") then "function" elif index("--method") then "method" else "symbol" end) as $kind |
        (if index("--delivered") then "delivered" else "not-delivered" end) as $mode |
        ([.[] | select(startswith("-") | not)][0] // "") as $sym |
        "\($sym)\t\($kind)\t\($mode)"
    ' "$batch_file")
    syms_only=$(awk -F'\t' 'NF >= 2 && $1 != "" { print $1 "\t" $2 }' <<<"$reqs_tsv" | sort -u)
    mapfile -t prod_files < <(collect_prod_files)
    all_hits=""
    if [ -n "$syms_only" ] && [ "${#prod_files[@]}" -gt 0 ]; then
        all_hits=$(scan_symbols_awk "$syms_only" "${prod_files[@]}")
    fi
    jq -n --arg reqs "$reqs_tsv" --arg hits "$all_hits" --arg root "$ROOT/" '
        ($hits | split("\n") | map(select(length > 0) | split("\t"))) as $hit_rows |
        [
            ($reqs | split("\n")[] | select(length > 0) | split("\t")) |
            .[0] as $sym | .[1] as $kind | .[2] as $mode |
            ([ $hit_rows[] | select(.[0] == $kind and .[1] == $sym) ]) as $sites |
            if $mode == "delivered" then
                if ($sites | length) > 0 then
                    {ok: true, output: "✅ reachability: `\($sym)` is called on the production path (\($sites | length) site(s))"}
                else
                    {ok: false, output: "❌ reachability: `\($sym)` has 0 production call sites — the delivered verdict no longer holds"}
                end
            else
                if ($sites | length) == 0 then
                    {ok: true, output: "✅ reachability: `\($sym)` still has 0 production call sites (refutation holds)"}
                else
                    {ok: false, output: "❌ reachability: `\($sym)` now has \($sites | length) production call site(s) — re-verify this vector and update its verdict"}
                end
            end
        ]
    '
    exit 0
fi

kind="symbol"
mode="not-delivered"
symbol=""
in_dirs=()
while [ "$#" -gt 0 ]; do
    case "$1" in
    --function) kind="function"; shift ;;
    --method) kind="method"; shift ;;
    --delivered) mode="delivered"; shift ;;
    --not-delivered|--refuted) mode="not-delivered"; shift ;;
    --in)
        shift
        while [ "$#" -gt 0 ] && [[ "$1" != --* ]]; do
            in_dirs+=("$ROOT/$1")
            shift
        done
        ;;
    -*)
        echo "usage: check-reachability.sh <symbol> [--function|--method] [--delivered|--not-delivered] [--in <dir> ...]" >&2
        exit 2
        ;;
    *)
        symbol=$1
        shift
        ;;
    esac
done

if [ -z "$symbol" ]; then
    echo "usage: check-reachability.sh <symbol> [--function|--method] [--delivered|--not-delivered] [--in <dir> ...]" >&2
    exit 2
fi

if [ "${#in_dirs[@]}" -gt 0 ]; then
    mapfile -t prod_files < <(collect_prod_files "${in_dirs[@]}")
else
    mapfile -t prod_files < <(collect_prod_files)
fi

hits=""
if [ "${#prod_files[@]}" -gt 0 ]; then
    hits=$(scan_symbols_awk "$(printf '%s\t%s\n' "$symbol" "$kind")" "${prod_files[@]}")
fi

count=0
if [ -n "$hits" ]; then
    count=$(grep -c '^' <<<"$hits" || echo 0)
fi

if [ "$mode" = "delivered" ]; then
    if [ "$count" -gt 0 ]; then
        echo "✅ $NAME: \`$symbol\` is called on the production path ($count site(s)):"
        awk -F'\t' -v root="$ROOT/" '{ sub("^" root, "", $3); printf "   %s:%s: %s\n", $3, $4, $5 }' <<<"$hits" | head -n 5
        exit 0
    fi
    echo "❌ $NAME: \`$symbol\` has 0 production call sites — the delivered verdict no longer holds"
    exit 1
else
    if [ "$count" -eq 0 ]; then
        echo "✅ $NAME: \`$symbol\` still has 0 production call sites (refutation holds)"
        exit 0
    fi
    echo "❌ $NAME: \`$symbol\` now has $count production call site(s) — re-verify this vector and update its verdict:"
    awk -F'\t' -v root="$ROOT/" '{ sub("^" root, "", $3); printf "   %s:%s: %s\n", $3, $4, $5 }' <<<"$hits" | head -n 5
    exit 1
fi
