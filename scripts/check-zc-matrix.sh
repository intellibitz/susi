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

jq -e '
  (["linux-cpu", "macos-cpu", "windows-cpu"] - [.profiles[].id] | length == 0) and
  ([.profiles[] | (.runner | type == "string" and length > 0) and .expect.installer_plan == true and .expect.lifecycle_smoke == true] | all) and
  (.journey.script | type == "string" and length > 0) and
  (.journey.budget_seconds | type == "number")
' "$MATRIX" >/dev/null || { echo "invalid matrix definition in $MATRIX" >&2; exit 1; }
echo "matrix definition: ok"

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
