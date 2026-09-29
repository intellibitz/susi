#!/usr/bin/env bash
# Acceptance check for "tests are hermetic against an inherited SUSI_HOME".
# A dev susi exports SUSI_HOME=~/.susi-dev; tests that isolate only HOME/XDG then
# read and write the launching instance. Run the whole workspace suite with a
# throwaway SUSI_HOME: every test must pass AND nothing may be written into it.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
home=$(mktemp -d)
trap 'rm -rf "$home"' EXIT
SUSI_HOME="$home" cargo test --workspace --locked --no-fail-fast
if [ -n "$(ls -A "$home")" ]; then
    echo "tests wrote into the inherited SUSI_HOME: $(ls -A "$home" | tr '\n' ' ')" >&2
    exit 1
fi
echo "hermetic: all tests passed and SUSI_HOME stayed empty"
