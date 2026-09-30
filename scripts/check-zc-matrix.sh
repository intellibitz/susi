#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
matrix=.agents/zc-matrix/profiles.json
test -f "$matrix"
python3 - <<'PY'
import json
from pathlib import Path
m=json.loads(Path(".agents/zc-matrix/profiles.json").read_text())
ids={p["id"] for p in m["profiles"]}
need={"cpu-only","emulated-gpu","low-memory"}
missing=need-ids
assert not missing, f"missing profiles: {missing}"
for p in m["profiles"]:
    assert "expect" in p and p["expect"], p
print("zc-matrix: ok", sorted(ids))
PY
