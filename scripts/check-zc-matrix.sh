#!/usr/bin/env bash
# Fresh-environment matrix contract: .github/zc-matrix/matrix.json defines
# the clean-container profiles (cpu-only, emulated-gpu, low-memory), each
# with an image, resource bounds and expected journey outcomes; the CI
# workflow runs the journey per profile.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MATRIX="$ROOT/.github/zc-matrix/matrix.json"
WF="$ROOT/.github/workflows/zc-matrix.yml"

[[ -f "$MATRIX" ]] || { echo "missing .github/zc-matrix/matrix.json" >&2; exit 1; }
[[ -f "$WF" ]] || { echo "missing .github/workflows/zc-matrix.yml" >&2; exit 1; }

python3 - "$MATRIX" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
profiles = {p["id"]: p for p in m["profiles"]}
required = {"cpu-only", "emulated-gpu", "low-memory"}
assert required <= set(profiles), f"missing profiles: {required - set(profiles)}"
for pid, p in profiles.items():
    assert p.get("image"), f"{pid}: no container image"
    assert isinstance(p.get("memory_mb"), int) and p["memory_mb"] > 0, f"{pid}: memory_mb"
    assert "expect" in p, f"{pid}: no expected outcomes"
    e = p["expect"]
    assert e.get("first_answer") is True, f"{pid}: must expect a first answer"
    assert "local_model" in e or "offline_fallback" in e, f"{pid}: no model/fallback expectation"
assert m["journey"]["script"], "journey script missing"
assert isinstance(m["journey"]["budget_seconds"], int), "journey budget missing"
print("matrix definition: ok")
PY

# The workflow must run the journey per profile.
grep -q 'matrix:' "$WF"
grep -q 'cpu-only' "$WF"
grep -q 'emulated-gpu' "$WF"
grep -q 'low-memory' "$WF"
grep -q 'zc_time_to_answer' "$WF"
grep -q 'check-zc-matrix.sh' "$WF"
echo "check-zc-matrix: ok"
