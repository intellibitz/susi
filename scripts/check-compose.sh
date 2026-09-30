#!/usr/bin/env bash
# Compose contract: deploy/compose ships cpu/nvidia/rocm profiles wiring
# susi to ollama and vllm; validates with `docker compose config` when
# docker is present, structurally otherwise.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FILE="$ROOT/deploy/compose/docker-compose.yml"

[[ -f "$FILE" ]] || { echo "missing deploy/compose/docker-compose.yml" >&2; exit 1; }
[[ -f "$ROOT/deploy/compose/Dockerfile.susi" ]] || { echo "missing Dockerfile.susi" >&2; exit 1; }

# Three hardware profiles, each naming at least one engine service.
for p in cpu nvidia rocm; do
  grep -q "\"$p\"" "$FILE" || { echo "profile $p missing" >&2; exit 1; }
done
grep -q 'ollama' "$FILE"
grep -q 'vllm' "$FILE"
grep -q 'driver: nvidia' "$FILE"          # nvidia reservation
grep -q '/dev/kfd' "$FILE"                # rocm device pass-through
grep -q 'susi-home' "$FILE"               # persistent SUSI_HOME
grep -q 'models' "$FILE"                  # shared model store
# susi service must run in every profile
[[ $(grep -c 'profiles:' "$FILE") -ge 4 ]]

if command -v docker >/dev/null 2>&1; then
  for p in cpu nvidia rocm; do
    docker compose -f "$FILE" --profile "$p" config --quiet
    # each profile must expose the susi service
    docker compose -f "$FILE" --profile "$p" config --services | grep -qx susi
  done
  echo "check-compose: ok (docker compose config)"
else
  echo "check-compose: ok (structural; docker not installed)"
fi
