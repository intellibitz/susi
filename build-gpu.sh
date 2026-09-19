#!/usr/bin/env sh
# SUSI GPU-Aware Manual Build

set -eu
echo "[build-gpu] Dispatching to cargo xtask..."

# We execute cargo xb (which points to xtask) overriding the need for this script!
# Wait, xtask ALREADY has the GPU logic.
# So build-gpu.sh just becomes a seamless pass-through to cargo xb!
exec cargo xb --release "$@"
