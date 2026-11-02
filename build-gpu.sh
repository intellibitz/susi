#!/usr/bin/env sh
# Compatibility entrypoint: CPU on Linux/Windows; optional Metal on macOS.
# CUDA/MKL product backends are retired. Explicit retired features fail in Cargo.
# This only builds; it does not install or qualify a release.
set -eu
exec cargo xb --release "$@"
