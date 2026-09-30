#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
file=deploy/compose/docker-compose.yml
test -f "$file"
# Prefer docker compose config when available; else structural check.
if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
  docker compose -f "$file" --profile cpu config >/dev/null
  docker compose -f "$file" --profile nvidia config >/dev/null
  docker compose -f "$file" --profile rocm config >/dev/null
else
  grep -q 'profiles: \["cpu", "nvidia", "rocm"\]' "$file" || grep -q 'cpu' "$file"
  grep -q ollama "$file"
  grep -q vllm "$file"
fi
echo "compose: ok"
