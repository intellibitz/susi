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
#
# Runner: `cargo test` by default. SUSI_HERMETIC_RUNNER=nextest runs the tests
# under cargo-nextest instead - one process per test, so a failing test can be
# retried alone (SUSI_HERMETIC_RETRIES, default 2; a test that passes on retry is
# reported as flaky, not failed) - and then the doc-tests, which nextest does not
# run, for the selected crates that have a library. This is the local finish
# gate's single test run: hermetic and retried in one pass, not a plain run
# followed by a hermetic repeat.
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
runner="${SUSI_HERMETIC_RUNNER:-cargo-test}"
case "$runner" in
cargo-test)
    if [ $# -gt 0 ]; then
        cargo test --locked --no-fail-fast "$@"
    else
        cargo test --workspace --locked --no-fail-fast
    fi
    ;;
nextest)
    if [ $# -gt 0 ]; then scope=("$@"); else scope=(--workspace); fi
    # A crate with no tests (most vendor shims) is a pass, as it is under cargo test.
    cargo nextest run --locked --no-fail-fast --no-tests=pass \
        --retries "${SUSI_HERMETIC_RETRIES:-2}" "${scope[@]}"
    # Doc-tests: `-p` on a package with no library target is an error, so name only
    # the selected packages that have one. A selection with no `-p` (`--workspace`)
    # is passed through: cargo skips the bin-only members itself.
    wanted=()
    prev=""
    for arg in "${scope[@]}"; do
        if [ "$prev" = -p ]; then wanted+=("$arg"); fi
        prev="$arg"
    done
    if [ "${#wanted[@]}" -eq 0 ]; then
        cargo test --locked --doc "${scope[@]}"
    else
        libs=$(cargo metadata --no-deps --format-version 1 | jq -r --argjson want \
            "$(printf '%s\n' "${wanted[@]}" | jq -R . | jq -s .)" '
            .packages[]
            | select(.name as $n | $want | index($n))
            | select(any(.targets[]; any(.kind[]; . == "lib" or . == "rlib" or . == "proc-macro")))
            | .name')
        doc_scope=()
        for lib in $libs; do doc_scope+=(-p "$lib"); done
        if [ "${#doc_scope[@]}" -gt 0 ]; then
            cargo test --locked --doc "${doc_scope[@]}"
        fi
    fi
    ;;
*)
    echo "SUSI_HERMETIC_RUNNER must be cargo-test or nextest, not '$runner'" >&2
    exit 2
    ;;
esac
if [ -n "$(ls -A "$home")" ]; then
    echo "tests wrote into the inherited SUSI_HOME: $(ls -A "$home" | tr '\n' ' ')" >&2
    exit 1
fi
echo "hermetic: all tests passed and SUSI_HOME stayed empty"
