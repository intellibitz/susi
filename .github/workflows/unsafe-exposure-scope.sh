#!/usr/bin/env bash
set -euo pipefail

# Read changed repository paths on stdin and print true when one can affect
# cargo-geiger --forbid-only or the unsafe-exposure baseline. That mode scans
# Cargo target entry points, not every Rust module, so use Cargo target paths
# from the manifests to distinguish entry points from ordinary modules.
# Missing Python/TOML support or malformed input is ambiguous and fails open.
root=${1:-.}
root=$(cd "$root" && pwd)
changed=$(cat)
[ -n "$changed" ] || {
  echo false
  exit 0
}

if grep -Eq '(^|/)Cargo\.toml$|^Cargo\.lock$|^rust-toolchain(\.toml)?$|^\.cargo/config(\.toml)?$|^\.agents/baseline/unsafe-default\.json$|^vendor/|^\.github/workflows/unsafe-exposure-(ratchet|scope)\.sh$' <<<"$changed"; then
  echo true
  exit 0
fi

rust_changed=$(grep -E '\.rs$' <<<"$changed" || true)
[ -n "$rust_changed" ] || {
  echo false
  exit 0
}

command -v python3 >/dev/null 2>&1 && python3 -c 'import tomllib' >/dev/null 2>&1 || {
  echo true
  exit 0
}

result=$(RUST_CHANGED="$rust_changed" python3 - "$root" <<'PY'
import glob
import os
import pathlib
import sys
import tomllib

root = pathlib.Path(sys.argv[1]).resolve()
changed = set(os.environ["RUST_CHANGED"].splitlines())
targets = set()

def add_path(directory, value):
    path = (directory / value).resolve()
    try:
        targets.add(path.relative_to(root).as_posix())
    except ValueError:
        # A target outside the workspace is unusual; fail open if changed.
        targets.add(path.as_posix())

def auto_entries(directory):
    if not directory.exists():
        return []
    return [
        *directory.glob("*.rs"),
        *(path for path in directory.glob("**/main.rs") if path.parent != directory),
    ]

for manifest in root.glob("**/Cargo.toml"):
    if any(part in {".git", "target", "node_modules"} for part in manifest.parts):
        continue
    directory = manifest.parent
    try:
        document = tomllib.loads(manifest.read_text(encoding="utf-8"))
        package = document.get("package", {})
    except Exception:
        print("true")
        raise SystemExit(0)

    lib = document.get("lib", {})
    if package.get("autolib", True):
        lib_path = lib.get("path", "src/lib.rs")
        if (directory / lib_path).is_file():
            add_path(directory, lib_path)

    for kind, auto_key, source_dir, default_file in (
        ("bin", "autobins", "src/bin", "src/main.rs"),
        ("example", "autoexamples", "examples", None),
        ("test", "autotests", "tests", None),
        ("bench", "autobenches", "benches", None),
    ):
        if package.get(auto_key, True):
            if default_file and (directory / default_file).is_file():
                add_path(directory, default_file)
            for target in auto_entries(directory / source_dir):
                add_path(directory, target.relative_to(directory).as_posix())
        declared = document.get(kind, [])
        if isinstance(declared, dict):
            declared = [declared]
        for target in declared:
            if isinstance(target, dict):
                add_path(directory, target.get("path", f"{source_dir}/{target['name']}.rs"))

    build = package.get("build", "build.rs")
    if build is not False and build and (directory / build).is_file():
        add_path(directory, build if isinstance(build, str) else "build.rs")

print("true" if changed & targets else "false")
PY
) || {
  echo true
  exit 0
}
echo "$result"
