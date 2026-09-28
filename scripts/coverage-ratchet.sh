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
# Usage: scripts/coverage-ratchet.sh            # measure + compare
#        scripts/coverage-ratchet.sh --update   # rewrite baselines to the
#                                              # measured values (review the
#                                              # diff — never lower a floor)
set -euo pipefail
cd "$(dirname "$0")/.."

cargo llvm-cov --workspace --summary-only > /tmp/susi-cov.txt
cargo llvm-cov -p susi-leaf-services --all-features --summary-only > /tmp/susi-cov-leaf.txt

python3 - "$@" <<'EOF'
import json, math, re, sys, collections

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

agg = parse('/tmp/susi-cov.txt')
# Merge the all-features leaf-services pass over the default-feature row.
for crate, (t, m) in parse('/tmp/susi-cov-leaf.txt').items():
    if crate == 'susi-leaf-services':
        agg[crate] = [t, m]

baseline = json.load(open('.agents/coverage-baseline.json'))['floor_percent']
measured = {c: 100.0 * (t - m) / t for c, (t, m) in agg.items() if t}

if '--update' in sys.argv:
    out = {c: round(math.floor(v), 1) for c, v in measured.items()}
    json.dump(
        {"_comment": "per-crate line-coverage floor; scripts/coverage-ratchet.sh fails CI when a crate drops below its baseline. Raise floors as coverage climbs toward 100%.",
         "floor_percent": out},
        open('.agents/coverage-baseline.json', 'w'), indent=1)
    print('baselines rewritten to measured floors — review the diff')
    sys.exit(0)

failed = []
for crate, floor in sorted(baseline.items()):
    got = measured.get(crate)
    if got is None:
        print(f'note: {crate} produced no instrumented lines (facade?) — skipped')
        continue
    ok = got + 0.05 >= floor
    print(f'{crate:32s} floor {floor:5.1f}%  measured {got:5.1f}%  {"ok" if ok else "REGRESSED"}')
    if not ok:
        failed.append(crate)

for crate in sorted(set(measured) - set(baseline)):
    print(f'note: {crate} not in baseline — add its floor')

if failed:
    print(f'\ncoverage regression in: {", ".join(failed)}', file=sys.stderr)
    sys.exit(1)
total = sum(v[0] for v in agg.values())
miss = sum(v[1] for v in agg.values())
print(f'\nworkspace line coverage: {100.0 * (total - miss) / total:.2f}%')
EOF
