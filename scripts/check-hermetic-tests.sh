#!/usr/bin/env bash
# Acceptance check for "tests are hermetic against an inherited SUSI_HOME".
# A dev susi exports SUSI_HOME=~/.susi-dev; tests that isolate only HOME/XDG then
# read and write the launching instance. Run the whole workspace suite with a
# throwaway SUSI_HOME: every test must pass AND nothing may be written into it.
#
# Mandate 52: `SUSI_HERMETIC_FORBIDDEN` marks that throwaway so `SusiDirs`
# ignores it (unit tests resolve under the throwaway HOME/XDG instead). A test
# that sets SUSI_HOME to a *different* path still gets an instance root.
#
# Keep CARGO_HOME/RUSTUP_HOME on the real developer dirs: rustup shims resolve
# toolchains via those, and crown verify needs rustc/wasm32-wasip1 on PATH.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
real_home="${HOME}"
home=$(mktemp -d)
testhome=$(mktemp -d)
trap 'rm -rf "$home" "$testhome"' EXIT
export CARGO_HOME="${CARGO_HOME:-$real_home/.cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-$real_home/.rustup}"
# The real cargo shim must stay reachable after HOME is replaced.
export PATH="${CARGO_HOME}/bin:${PATH}"
export HOME="$testhome"
export USERPROFILE="$testhome"
export XDG_CONFIG_HOME="$testhome/xdg-config"
export XDG_DATA_HOME="$testhome/xdg-data"
export XDG_CACHE_HOME="$testhome/xdg-cache"
export SUSI_HOME="$home"
export SUSI_HERMETIC_FORBIDDEN="$home"
# Drop a launch-time port offset the same way a real inherited env might set it.
unset SUSI_PORT_OFFSET || true
# Scope it when arguments are given (`-p susi-gawd`), so the rule can be enforced
# per affected crate instead of only as a whole-workspace sweep nobody runs.
if [ $# -gt 0 ]; then
    cargo test --locked --no-fail-fast "$@"
else
    cargo test --workspace --locked --no-fail-fast
fi
if [ -n "$(ls -A "$home")" ]; then
    echo "tests wrote into the inherited SUSI_HOME: $(ls -A "$home" | tr '\n' ' ')" >&2
    exit 1
fi
echo "hermetic: all tests passed and SUSI_HOME stayed empty"
