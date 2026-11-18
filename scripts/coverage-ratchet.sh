#!/usr/bin/env bash
# Per-crate line-coverage ratchet (AGENTS.md test policy).
#
# Runs cargo-llvm-cov over the workspace, aggregates line coverage per crate,
# and fails when any crate drops below its floor in
# .agents/coverage-baseline.json. Floors only move up: raise them in the same
# commit that lands the tests (the goal is 100%).
#
# susi-leaf-services compiles its service shells only under --all-features
# (workspace-wide --all-features is unsafe: cuda/mkl/metal would collide), so
# that crate is measured by a second pass and merged over the default row.
#
# Coverage is genuinely nondeterministic where tests gate on the host env
# (GGUF weights, CUDA, live services, ~/.susi state, env-var races):
# susi-paths measured 81.4% and 74.3% on identical code in consecutive CI
# runs (EV-DEVIN-060). A regression therefore gets one re-measure and each
# crate keeps its best run — a real regression stays below the floor in
# every run, a flake does not. Retry cost is paid only on failure (the
# instrumented build is already done; only the tests re-execute).
#
# Usage: scripts/coverage-ratchet.sh            # measure + compare
#        scripts/coverage-ratchet.sh --update   # rewrite baselines to the
#                                              # measured values (review the
#                                              # diff — never lower a floor)
set -euo pipefail
cd "$(dirname "$0")/.."

# The coverage report is large and the runner's /tmp is a small tmpfs; honor
# TMPDIR (the workflow points it at the big disk) so report generation never
# hits EDQUOT.
TMP="${TMPDIR:-/tmp}"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target/llvm-cov-target}"

run_cov() {
    local pkgs_arg="${SUSI_COV_PKGS:-}"
    if [ -z "$pkgs_arg" ]; then
        pkgs_arg="--workspace"
    fi

    if command -v cargo-nextest >/dev/null 2>&1; then
        cargo llvm-cov nextest $pkgs_arg --retries 2 --no-fail-fast > "$1"
        if [ "$pkgs_arg" = "--workspace" ] || [[ "$pkgs_arg" =~ "susi-leaf-services" ]]; then
            cargo llvm-cov nextest -p susi-leaf-services --all-features --retries 2 --no-fail-fast > "$2"
        else
            : > "$2"
        fi
    else
        export RUST_TEST_THREADS="${RUST_TEST_THREADS:-4}"
        cargo llvm-cov $pkgs_arg --summary-only > "$1"
        if [ "$pkgs_arg" = "--workspace" ] || [[ "$pkgs_arg" =~ "susi-leaf-services" ]]; then
            cargo llvm-cov -p susi-leaf-services --all-features --summary-only > "$2"
        else
            : > "$2"
        fi
    fi
}

# ratchet <mode> <ws1> <leaf1> [ws2 leaf2 ...] — each pair is one
# measurement run; per-crate coverage is the max across runs.
ratchet() {
    local mode=$1
    shift
    if [ "$#" -lt 2 ] || [ $(( $# % 2 )) -ne 0 ]; then
        echo "expected ws/leaf file pairs" >&2
        return 1
    fi
    local baseline_tsv
    baseline_tsv=$(jq -r '.floor_percent | to_entries | sort_by(.key)[] | "\(.key)\t\(.value)"' .agents/coverage-baseline.json)
    if [ "$mode" = "--update" ]; then
        local updates_json
        updates_json=$(awk '
            function crate_of(path,    parts) {
                if (path ~ /^crates\/[^\/]+\//) {
                    split(path, parts, "/")
                    return parts[2]
                }
                if (path ~ /^src\//) return "susi"
                if (path ~ /^xtask\//) return "xtask"
                return ""
            }
            {
                c = crate_of($1)
                if (c != "" && $2 ~ /^[0-9]+$/ && $3 ~ /^[0-9]+$/ && $4 ~ /^[0-9.]+%$/) {
                    if (ARGIND % 2 == 1) {
                        t[ARGIND, c] += $2
                        m[ARGIND, c] += $3
                        crates[c] = 1
                    } else if (c == "susi-leaf-services") {
                        lt[ARGIND - 1, c] += $2
                        lm[ARGIND - 1, c] += $3
                        crates[c] = 1
                    }
                }
            }
            END {
                for (i = 1; i < ARGC; i += 2) {
                    if (lt[i, "susi-leaf-services"] > 0) {
                        t[i, "susi-leaf-services"] = lt[i, "susi-leaf-services"]
                        m[i, "susi-leaf-services"] = lm[i, "susi-leaf-services"]
                    }
                    for (c in crates) {
                        if (t[i, c] > 0) {
                            pct = 100.0 * (t[i, c] - m[i, c]) / t[i, c]
                            if (!(c in best) || pct > best[c]) best[c] = pct
                        }
                    }
                }
                for (c in best) {
                    printf "%s\t%.1f\n", c, int(best[c])
                }
            }
        ' "$@")
        jq --arg rows "$updates_json" '
            .floor_percent = (
                $rows
                | split("\n")
                | map(select(length > 0) | split("\t") | {key: .[0], value: (.[1] | tonumber)})
                | from_entries
            )
        ' .agents/coverage-baseline.json > .agents/coverage-baseline.json.tmp
        mv .agents/coverage-baseline.json.tmp .agents/coverage-baseline.json
        echo "baselines rewritten to measured floors — review the diff"
        return 0
    fi

    BASELINES="$baseline_tsv" awk '
        function crate_of(path,    parts) {
            if (path ~ /^crates\/[^\/]+\//) {
                split(path, parts, "/")
                return parts[2]
            }
            if (path ~ /^src\//) return "susi"
            if (path ~ /^xtask\//) return "xtask"
            return ""
        }
        BEGIN {
            n_base = split(ENVIRON["BASELINES"], blines, "\n")
            for (i = 1; i <= n_base; i++) {
                if (blines[i] == "") continue
                split(blines[i], kv, "\t")
                base_order[++base_count] = kv[1]
                floor[kv[1]] = kv[2] + 0.0
            }
        }
        {
            run_idx = int((ARGIND - 1) / 2) + 1
            if (run_idx > num_runs) num_runs = run_idx
            c = crate_of($1)
            if (c != "" && $2 ~ /^[0-9]+$/ && $3 ~ /^[0-9]+$/ && $4 ~ /^[0-9.]+%$/) {
                if (ARGIND % 2 == 1) {
                    t[run_idx, c] += $2
                    m[run_idx, c] += $3
                    crates[c] = 1
                } else if (c == "susi-leaf-services") {
                    lt[run_idx, c] += $2
                    lm[run_idx, c] += $3
                    crates[c] = 1
                }
            }
        }
        END {
            for (r = 1; r <= num_runs; r++) {
                if (lt[r, "susi-leaf-services"] > 0) {
                    t[r, "susi-leaf-services"] = lt[r, "susi-leaf-services"]
                    m[r, "susi-leaf-services"] = lm[r, "susi-leaf-services"]
                }
                for (c in crates) {
                    if (t[r, c] > 0) {
                        pct = 100.0 * (t[r, c] - m[r, c]) / t[r, c]
                        run_pct[r, c] = pct
                        has_measured[c] = 1
                        if (!(c in best) || pct > best[c]) best[c] = pct
                    }
                }
            }
            failed_list = ""
            for (i = 1; i <= base_count; i++) {
                c = base_order[i]
                fl = floor[c]
                if (!(c in has_measured)) {
                    printf "note: %s produced no instrumented lines (facade?) — skipped\n", c
                    continue
                }
                got = best[c]
                ok = (got + 0.05 >= fl)
                detail = ""
                if (num_runs > 1) {
                    detail = "  (best of " num_runs " runs: "
                    for (r = 1; r <= num_runs; r++) {
                        if (r > 1) detail = detail ", "
                        detail = detail sprintf("%.1f", run_pct[r, c] + 0.0)
                    }
                    detail = detail ")"
                }
                printf "%-32s floor %5.1f%%  measured %5.1f%%  %s%s\n", c, fl, got, (ok ? "ok" : "REGRESSED"), detail
                if (!ok) {
                    failed_list = (failed_list == "" ? c : failed_list ", " c)
                }
            }
            for (c in has_measured) {
                if (!(c in floor)) {
                    printf "note: %s not in baseline — add its floor\n", c
                }
            }
            if (failed_list != "") {
                printf "\ncoverage regression in: %s\n", failed_list > "/dev/stderr"
                exit 1
            }
            total = 0
            miss = 0
            for (c in crates) {
                total += t[num_runs, c]
                miss += m[num_runs, c]
            }
            if (total > 0) {
                printf "\nline coverage: %.2f%%\n", 100.0 * (total - miss) / total
            }
        }
    ' "$@"
}

if [[ "${1:-}" == "--update" ]]; then
    run_cov "$TMP/susi-cov.txt" "$TMP/susi-cov-leaf.txt"
    ratchet --update "$TMP/susi-cov.txt" "$TMP/susi-cov-leaf.txt"
    exit 0
fi

if ! run_cov "$TMP/susi-cov.txt" "$TMP/susi-cov-leaf.txt" || ! ratchet --check "$TMP/susi-cov.txt" "$TMP/susi-cov-leaf.txt"; then
    echo "coverage regression measured — re-running once to rule out env flake"
    run_cov "$TMP/susi-cov-r2.txt" "$TMP/susi-cov-leaf-r2.txt" || true
    ratchet --check \
        "$TMP/susi-cov.txt" "$TMP/susi-cov-leaf.txt" \
        "$TMP/susi-cov-r2.txt" "$TMP/susi-cov-leaf-r2.txt"
fi
