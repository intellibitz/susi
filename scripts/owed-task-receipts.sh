#!/usr/bin/env bash
# Compatibility entrypoint only. Native callers run the Cargo binary directly;
# receipt parsing and Git queries have one implementation on Windows and Linux.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
exec cargo run --quiet --offline --locked --manifest-path "$root/Cargo.toml" \
    --package susi-unsafe-ratchet --bin owed-task-receipts -- "$@"
