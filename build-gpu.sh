#!/usr/bin/env sh
# SUSI GPU-Aware Manual Build
#
# A bare `cargo build --release` silently produces a CPU-only binary even on
# a host with a real GPU — `cuda`/`metal` are opt-in Cargo features, not
# something Cargo can auto-detect from the host. A live benchmark measured a
# 17x tokens/sec difference between the two on identical hardware
# (EV-2022920-035, .agents/EVIDENCE.md). This script is the manual-build
# equivalent of the GPU detection install.sh already does for the one-liner
# install path, so a contributor building from source gets the same
# GPU-aware behavior without needing to know cudarc's version-ceiling quirk.
#
# Usage: ./build-gpu.sh [additional cargo build args]

set -eu

PLATFORM="$(uname -s)"
BUILD_FEATURES=""

if [ "$PLATFORM" = "Darwin" ]; then
    BUILD_FEATURES="--features metal"
elif command -v nvcc >/dev/null 2>&1 || [ -d "/usr/local/cuda" ]; then
    CUDA_VERSION=$(nvcc --version 2>/dev/null | grep "release" | sed 's/.*release //;s/,.*//' || echo "0")
    case "$CUDA_VERSION" in
        11.*|12.*|13.*)
            BUILD_FEATURES="--features cuda"
            # cudarc (candle's CUDA backend) pins an exact allowlist of CUDA
            # toolkit versions and panics on any newer point release it hasn't
            # added yet (as of cudarc 0.19.9, that ceiling is 13.3). Clamp
            # newer CUDA 13.x releases down to 13.3 via cudarc's own override
            # env var — CUDA maintains ABI compatibility within a major
            # version, so this is safe rather than the build failing outright.
            CUDA_MAJOR="${CUDA_VERSION%%.*}"
            CUDA_MINOR="${CUDA_VERSION#*.}"
            if [ "$CUDA_MAJOR" = "13" ] && echo "$CUDA_MINOR" | grep -qE '^[0-9]+$' && [ "$CUDA_MINOR" -gt 3 ]; then
                export CUDARC_CUDA_VERSION="13030"
            fi
            ;;
    esac
fi

if [ -z "$BUILD_FEATURES" ]; then
    echo "[build-gpu] No GPU backend detected (or unsupported on this platform) — building CPU-only."
else
    echo "[build-gpu] GPU backend detected: building with $BUILD_FEATURES"
fi

# shellcheck disable=SC2086
exec cargo build --release $BUILD_FEATURES "$@"
