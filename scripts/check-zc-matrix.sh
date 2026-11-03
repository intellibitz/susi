#!/usr/bin/env bash
# Fresh-environment matrix contract: .github/zc-matrix/matrix.json defines
# the per-OS lifecycle-smoke profiles (linux-cpu, macos-cpu, windows-cpu),
# each with a runner and expected journey outcomes; the CI workflows run
# the journey per profile on that OS family. Per the CI platform policy (CEO
# decision 2026-11-03, AGENTS.md) the release platforms' profiles run per
# change in zc-matrix.yml and macos-cpu runs nightly in nightly.yml.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MATRIX="$ROOT/.github/zc-matrix/matrix.json"
WF="$ROOT/.github/workflows/zc-matrix.yml"
NIGHTLY="$ROOT/.github/workflows/nightly.yml"

[[ -f "$MATRIX" ]] || { echo "missing .github/zc-matrix/matrix.json" >&2; exit 1; }
[[ -f "$WF" ]] || { echo "missing .github/workflows/zc-matrix.yml" >&2; exit 1; }
[[ -f "$NIGHTLY" ]] || { echo "missing .github/workflows/nightly.yml" >&2; exit 1; }

jq -e '
  (["linux-cpu", "macos-cpu", "windows-cpu"] - [.profiles[].id] | length == 0) and
  ([.profiles[] | (.runner | type == "string" and length > 0) and .expect.installer_plan == true and .expect.lifecycle_smoke == true] | all) and
  (.journey.script | type == "string" and length > 0) and
  (.journey.budget_seconds | type == "number")
' "$MATRIX" >/dev/null || { echo "invalid matrix definition in $MATRIX" >&2; exit 1; }
echo "matrix definition: ok"

# Each workflow must run the journey for the profiles of its tier.
check_workflow() {
  local wf=$1
  shift
  grep -q 'matrix:' "$wf"
  for pid in "$@"; do
    grep -q "profile: $pid" "$wf" || { echo "$wf missing profile $pid" >&2; exit 1; }
  done
  grep -q 'zc_platform_installers' "$wf"
  grep -q 'vc_201_098_mastery' "$wf"
  grep -q 'SUSI_PROFILE' "$wf"
  grep -q 'check-zc-matrix.sh' "$wf"
}
check_workflow "$WF" linux-cpu windows-cpu
check_workflow "$NIGHTLY" macos-cpu
if grep -q 'profile: macos-cpu' "$WF"; then
  echo "$WF runs macos-cpu per change; it is a nightly profile (nightly.yml)" >&2
  exit 1
fi
echo "check-zc-matrix: ok"
