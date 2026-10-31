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

if grep -Eq '(^|/)Cargo\.toml$|^Cargo\.lock$|^rust-toolchain(\.toml)?$|^\.cargo/config(\.toml)?$|^\.agents/baseline/unsafe-default\.json$|^vendor/|^\.github/workflows/unsafe-exposure-ratchet\.sh$' <<<"$changed"; then
  echo true
  exit 0
fi

rust_changed=$(grep -E '\.rs$' <<<"$changed" || true)
[ -n "$rust_changed" ] || {
  echo false
  exit 0
}

# An unrecognized path outside a nested src/ module may be a Cargo target entry
# point (src/lib.rs, src/main.rs, src/bin/**, tests/**, examples/**, benches/**,
# build.rs) or a custom target path. Only a clearly nested module file under
# src/ inside a workspace crate without custom target path overrides is safe to
# skip; any ambiguity fails open and runs the full ratchet.
while IFS= read -r rel; do
  [ -n "$rel" ] || continue
  case "$rel" in
    */main.rs|main.rs|*/src/bin/*|src/bin/*)
      echo true
      exit 0
      ;;
  esac
  if [[ "$rel" =~ ^((crates/[^/]+|xtask|tools/[^/]+)/)?src/[^/]+/.+\.rs$ ]]; then
    pkg_dir="${BASH_REMATCH[2]:-.}"
    manifest="$root/$pkg_dir/Cargo.toml"
    if [ ! -f "$manifest" ]; then
      echo true
      exit 0
    fi
    rel_in_pkg="${rel#${pkg_dir}/}"
    if grep -Fq "$rel_in_pkg" "$manifest" 2>/dev/null; then
      echo true
      exit 0
    fi
  else
    echo true
    exit 0
  fi
done <<<"$rust_changed"

echo false
