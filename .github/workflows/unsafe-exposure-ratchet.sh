#!/usr/bin/env bash
set -euo pipefail

baseline="${1:-.agents/baseline/unsafe-default.json}"
command -v cargo-geiger >/dev/null || {
  echo "cargo-geiger is required for the unsafe-exposure ratchet" >&2
  exit 1
}
mkdir -p "$(dirname "$baseline")"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cargo geiger --output-format Json >"$tmp/geiger.json"
cargo metadata --format-version 1 --locked >"$tmp/metadata.json"

python3 - "$baseline" "$tmp/geiger.json" "$tmp/metadata.json" <<'PY'
import json
import pathlib
import sys

baseline_path, geiger_path, metadata_path = map(pathlib.Path, sys.argv[1:])
geiger = json.loads(geiger_path.read_text())
metadata = json.loads(metadata_path.read_text())
versions = {package["name"]: package["version"] for package in metadata["packages"]}
items = geiger.get("packages", geiger if isinstance(geiger, list) else [])
current = {}
for item in items:
    name = item.get("name") or item.get("package", {}).get("name")
    if not name:
        continue
    counts = item.get("unsafety", item.get("unsafe", {}))
    unsafe = counts.get("functions", counts.get("unsafe_functions", 0)) if isinstance(counts, dict) else 0
    current[name] = {"crate_name": name, "version": versions.get(name, "unknown"), "unsafe_fns": int(unsafe or 0)}

old = json.loads(baseline_path.read_text()).get("exposure", []) if baseline_path.exists() else []
previous = {item["crate_name"]: item["unsafe_fns"] for item in old}
violations = [name for name, item in current.items() if item["unsafe_fns"] > previous.get(name, 0)]
if violations:
    print("new unsafe exposure requires review:", ", ".join(sorted(violations)), file=sys.stderr)
    sys.exit(2)
print("unsafe-exposure ratchet clean")
PY
