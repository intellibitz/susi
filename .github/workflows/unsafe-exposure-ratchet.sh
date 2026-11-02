#!/usr/bin/env bash
set -euo pipefail

# Compatibility entry point for CI. Measurement and admission are native Rust;
# raw cargo-geiger exports cannot prove complete per-file measurement.
# Baseline migration is deliberately a separate evidence-bound native command.
baseline="${1:-.agents/baseline/unsafe-default.json}"
if [ "$#" -gt 1 ]; then
  echo 'baseline migration requires matched native scans and component review; use unsafe_ratchet --write-baseline with those inputs' >&2
  exit 2
fi
root=$(git rev-parse --show-toplevel)
target=$(rustc -vV | sed -n 's/^host: //p')
[ -n "$target" ] || { echo 'rustc did not identify its host' >&2; exit 2; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo run -q -p susi-unsafe-ratchet --offline --locked --bin unsafe_measure -- \
  --root "$root" --target "$target" --features default --output "$tmp/measurement.json"
cargo run -q -p susi-unsafe-ratchet --offline --locked --bin unsafe_ratchet -- \
  --measurement "$tmp/measurement.json" --baseline "$baseline" --receipt "$tmp/receipt.json"
