#!/usr/bin/env bash
set -euo pipefail

# Native declaration measurement records all selected crate entry points.
# Conservatively rerun it for any Rust/source/graph/policy input, including
# nested modules. This is not a claim to count unsafe functions in modules.
root=${1:-.}
[ -d "$root" ] || { echo true; exit 0; }
changed=$(cat)
[ -n "$changed" ] || { echo false; exit 0; }
if grep -Eq '\.rs$|(^|/)Cargo\.toml$|^Cargo\.lock$|^rust-toolchain(\.toml)?$|^\.cargo/config(\.toml)?$|^\.agents/baseline/unsafe-default\.json$|^\.agents/reviews/unsafe-complete/|^vendor/|^\.github/workflows/unsafe-exposure-(ratchet|scope)\.sh$' <<<"$changed"; then
  echo true
else
  echo false
fi
