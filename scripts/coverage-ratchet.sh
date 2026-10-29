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
    python3 - "$@" <<'PYEOF'
import json, math, re, sys, collections

mode = sys.argv[1]
pairs = sys.argv[2:]
assert len(pairs) >= 2 and len(pairs) % 2 == 0, 'expected ws/leaf file pairs'

def parse(path):
    agg = collections.defaultdict(lambda: [0, 0])
    for line in open(path, errors='ignore'):
        m = re.match(r'(crates/([^/]+)/\S+|src/\S+|xtask/\S+)\s+(\d+)\s+(\d+)\s+([\d.]+)%', line)
        if not m:
            continue
        crate = m.group(2) if m.group(2) else ('susi' if m.group(1).startswith('src/') else 'xtask')
        agg[crate][0] += int(m.group(3))
        agg[crate][1] += int(m.group(4))
    return agg

def measured_of(ws_path, leaf_path):
    agg = parse(ws_path)
    # Merge the all-features leaf-services pass over the default-feature row.
    for crate, (t, m) in parse(leaf_path).items():
        if crate == 'susi-leaf-services':
            agg[crate] = [t, m]
    return agg, {c: 100.0 * (t - m) / t for c, (t, m) in agg.items() if t}

runs = [measured_of(pairs[i], pairs[i + 1]) for i in range(0, len(pairs), 2)]
measured = {}
for _, run in runs:
    for crate, pct in run.items():
        measured[crate] = max(measured.get(crate, 0.0), pct)
agg = runs[-1][0]

doc = json.load(open('.agents/coverage-baseline.json'))
baseline = doc['floor_percent']

if mode == '--update':
    doc['floor_percent'] = {c: round(math.floor(v), 1) for c, v in measured.items()}
    with open('.agents/coverage-baseline.json', 'w') as f:
        json.dump(doc, f, indent=1)
        f.write('\n')
    print('baselines rewritten to measured floors — review the diff')
    sys.exit(0)

failed = []
for crate, floor in sorted(baseline.items()):
    got = measured.get(crate)
    if got is None:
        print(f'note: {crate} produced no instrumented lines (facade?) — skipped')
        continue
    ok = got + 0.05 >= floor
    detail = f'  (best of {len(runs)} runs: ' + ', '.join(
        f"{run.get(crate, 0.0):.1f}" for _, run in runs) + ')' if len(runs) > 1 else ''
    print(f'{crate:32s} floor {floor:5.1f}%  measured {got:5.1f}%  {"ok" if ok else "REGRESSED"}{detail}')
    if not ok:
        failed.append(crate)

for crate in sorted(set(measured) - set(baseline)):
    print(f'note: {crate} not in baseline — add its floor')

if failed:
    print(f'\ncoverage regression in: {", ".join(failed)}', file=sys.stderr)
    sys.exit(1)
total = sum(v[0] for v in agg.values())
miss = sum(v[1] for v in agg.values())
if total > 0:
    print(f'\nline coverage: {100.0 * (total - miss) / total:.2f}%')
PYEOF
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
