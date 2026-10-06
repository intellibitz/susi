#!/usr/bin/env bash
# Fresh-environment matrix contract: .github/zc-matrix/matrix.json defines
# the per-OS lifecycle-smoke profiles (linux-cpu, macos-cpu, windows-cpu),
# each with a runner and expected journey outcomes; the CI workflow runs
# the journey per profile on that OS family.
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
required = {"linux-cpu", "macos-cpu", "windows-cpu"}
assert required <= set(profiles), f"missing profiles: {required - set(profiles)}"
for pid, p in profiles.items():
    assert p.get("runner"), f"{pid}: no runner"
    assert "expect" in p, f"{pid}: no expected outcomes"
    e = p["expect"]
    assert e.get("installer_plan") is True, f"{pid}: must expect the installer plan"
    assert e.get("lifecycle_smoke") is True, f"{pid}: must expect the lifecycle smoke"
assert m["journey"]["script"], "journey script missing"
assert isinstance(m["journey"]["budget_seconds"], int), "journey budget missing"
print("matrix definition: ok")
PY

# The workflow must run the journey per declared profile.
grep -q 'matrix:' "$WF"
for pid in linux-cpu macos-cpu windows-cpu; do
  grep -q "$pid" "$WF" || { echo "workflow missing profile $pid" >&2; exit 1; }
done
grep -q 'zc_platform_installers' "$WF"
grep -q 'vc_201_098_mastery' "$WF"
grep -q 'SUSI_PROFILE' "$WF"
grep -q 'check-zc-matrix.sh' "$WF"
echo "check-zc-matrix: ok"
