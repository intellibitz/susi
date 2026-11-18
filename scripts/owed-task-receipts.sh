#!/usr/bin/env bash
# Compatibility entrypoint only. The native owed-receipt scanner moved with the
# unsafe-code ratchet to Susi Commercial Apps (susi-unsafe-ratchet); this
# forwards to that checkout, and names what serves it when there is none.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
commercial=${SUSI_COMMERCIAL_CHECKOUT:-$root/../susi-commercial-apps}
if [ ! -f "$commercial/crates/susi-unsafe-ratchet/Cargo.toml" ]; then
    echo "owed-task-receipts is served by Susi Commercial Apps: set SUSI_COMMERCIAL_CHECKOUT to its checkout (looked in $commercial)" >&2
    exit 2
fi
# By manifest path: that workspace also locks core's pinned copy of the crate.
exec cargo run --quiet --offline --locked \
    --manifest-path "$commercial/crates/susi-unsafe-ratchet/Cargo.toml" \
    --bin owed-task-receipts -- "$@"
