#!/usr/bin/env bash
set -euo pipefail

# Unsafe-exposure ratchet (VC-201-097): run cargo-geiger + SBOM generation
# for the pinned feature set, then gate through the production comparator
# `susi_gawd::unsafe_ratchet::ratchet_unsafe` via its binary. New exposure
# fails the job; the auditable review path is re-running with
# --write-baseline and committing the regenerated baseline diff.
#
#   unsafe-exposure-ratchet.sh [baseline-path] [--write-baseline]

baseline="${1:-.agents/baseline/unsafe-default.json}"
write_baseline=""
for arg in "${@:2}"; do
  case "$arg" in
    --write-baseline) write_baseline="1" ;;
    *) echo "unknown argument: $arg" >&2; exit 1 ;;
  esac
done

command -v cargo-geiger >/dev/null || {
  echo "cargo-geiger is required for the unsafe-exposure ratchet" >&2
  exit 1
}
mkdir -p "$(dirname "$baseline")"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cargo geiger --output-format Json >"$tmp/geiger.json"
cargo metadata --format-version 1 --locked >"$tmp/metadata.json"

if [ -n "$write_baseline" ]; then
  exec cargo run -q -p susi-gawd --locked --bin unsafe_ratchet -- \
    --geiger "$tmp/geiger.json" \
    --metadata "$tmp/metadata.json" \
    --baseline "$baseline" \
    --write-baseline "$baseline" \
    --features default
fi

exec cargo run -q -p susi-gawd --locked --bin unsafe_ratchet -- \
  --geiger "$tmp/geiger.json" \
  --metadata "$tmp/metadata.json" \
  --baseline "$baseline" \
  --features default
