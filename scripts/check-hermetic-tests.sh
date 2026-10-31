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
export SCCACHE_IGNORE_SERVER_IO_ERROR=${SCCACHE_IGNORE_SERVER_IO_ERROR:-1}
hermetic_started=$SECONDS
[ ! -f "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh" ] || . "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh"
cd "$(git rev-parse --show-toplevel)"
real_home="${HOME}"
# On Windows the throwaway homes, and the temp dir the tests get (below), live
# under the clone's common git dir (`.git/susi-hermetic/`, the primary
# checkout's, shared by every linked worktree), not the user's temp: the tests
# write thousands of small files there (scratch worktrees, copied fixture
# trees, instance homes), and the user's temp is scanned on every write by
# real-time protection while a developer's checkout is typically excluded from
# it (one I/O-bound test: 180 s in the user's temp, 141 s in the checkout, 16 s
# on a Linux runner). The git dir rather than the cargo target: `finish` stages
# a copy of the running susi.exe in the temp dir precisely because it is
# outside the target the gate rebuilds (src/cli/workflow_cli.rs). The place is
# as hermetic as the user's temp (the tests never look for it; git ignores
# what it did not write there), and the EXIT trap removes it. Linux keeps the
# system temp: scripts/kill-leftover-service-runs.sh recognises a test's
# daemon by a SUSI_HOME under it. SUSI_HERMETIC_TMP names another parent on
# any platform; one that cannot be written falls back to mktemp's default.
hermetic_parent=${SUSI_HERMETIC_TMP:-}
if [ -z "$hermetic_parent" ]; then
    case "${OSTYPE:-}" in
    msys* | cygwin*)
        hermetic_parent=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)/susi-hermetic
        ;;
    esac
fi
if [ -n "$hermetic_parent" ] && mkdir -p "$hermetic_parent" 2>/dev/null && [ -w "$hermetic_parent" ]; then
    home=$(mktemp -d "$hermetic_parent/home.XXXXXX")
    testhome=$(mktemp -d "$hermetic_parent/user.XXXXXX")
else
    home=$(mktemp -d)
    testhome=$(mktemp -d)
fi
cleanup_home=$(cd "$home" && pwd -P)
cleanup_testhome=$(cd "$testhome" && pwd -P)
cleanup_parent=$(dirname "$cleanup_home")
cleanup_testparent=$(dirname "$cleanup_testhome")
remove_hermetic_dirs() {
    local attempt=0
    # Only the two directories this invocation created, resolved before any
    # background work. Refuse a recursive removal outside their parents.
    case "$cleanup_home" in "$cleanup_parent"/*) ;; *) return 1 ;; esac
    case "$cleanup_testhome" in "$cleanup_testparent"/*) ;; *) return 1 ;; esac
    while [ "$attempt" -lt 6 ]; do
        if rm -rf -- "$cleanup_home" "$cleanup_testhome" 2>/dev/null; then
            return 0
        fi
        attempt=$((attempt + 1))
        sleep 1
    done
    echo "warning: could not remove hermetic temporary directories after retries" >&2
}
cleanup_hermetic_dirs() {
    local status=$? cleanup_started=$SECONDS
    trap - EXIT
    # Reclaim the leaf children of any daemon a test started: they hold the
    # test binary's stdout pipe and, on Windows, `target\debug\susi.exe`.
    "$(dirname "${BASH_SOURCE[0]}")/kill-leftover-service-runs.sh" || true
    # Service termination and the hermetic check remain synchronous. Deleting
    # their now-unused scratch trees is housekeeping, with no inherited streams.
    (trap - EXIT; trap '' HUP; remove_hermetic_dirs) \
        >>"$(git rev-parse --path-format=absolute --git-common-dir)/susi-hermetic-cleanup.log" 2>&1 </dev/null &
    echo "hermetic: service cleanup took $((SECONDS - cleanup_started))s; scratch deletion continues in background" >&2
    return "$status"
}
trap cleanup_hermetic_dirs EXIT
export CARGO_HOME="${CARGO_HOME:-$real_home/.cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-$real_home/.rustup}"
# The real cargo shim must stay reachable after HOME is replaced.
export PATH="${CARGO_HOME}/bin:${PATH}"
# The machine's compiler cache server, started from THIS environment before
# the switch below (scripts/setup-sccache.sh --ensure-server; a no-op when one
# runs, or where the cache was never set up): sccache's client starts a server
# when none runs, and that server keeps the client's environment. Started by
# this run's first cargo, it kept this run's throwaway TMP, wrote its temp
# files there after the EXIT trap removed it, and every compile on the machine
# failed with "Failed to create temp dir" until it idled out: another agent's
# gate was what failed.
if command -v sccache >/dev/null 2>&1 && [ -f "$(dirname "${BASH_SOURCE[0]}")/setup-sccache.sh" ]; then
    "${BASH:-bash}" "$(dirname "${BASH_SOURCE[0]}")/setup-sccache.sh" --ensure-server || true
fi
export HOME="$testhome"
export USERPROFILE="$testhome"
export XDG_CONFIG_HOME="$testhome/xdg-config"
export XDG_DATA_HOME="$testhome/xdg-data"
export XDG_CACHE_HOME="$testhome/xdg-cache"
case "${OSTYPE:-}" in
    msys*|cygwin*)
        # Windows path discovery uses APPDATA/LOCALAPPDATA, not XDG. Keep both
        # the Susi instance and native build-tool caches inside this run's temp
        # home so a hermetic test cannot see or mutate the user's AppData.
        mkdir -p "$testhome/appdata/roaming" "$testhome/appdata/local/ort-cache" \
            "$testhome/appdata/local/temp"
        export APPDATA="$(cygpath -aw "$testhome/appdata/roaming")"
        export LOCALAPPDATA="$(cygpath -aw "$testhome/appdata/local")"
        export ORT_CACHE_DIR="$(cygpath -aw "$testhome/appdata/local/ort-cache")"
        # Rust's std::env::temp_dir and native Windows tools honor these
        # variables. MSYS may export /tmp here, which native Git/Cargo tests
        # cannot use for temporary repositories or worktrees.
        export TMP="$(cygpath -aw "$testhome/appdata/local/temp")"
        export TEMP="$TMP"
        export TMPDIR="$testhome/appdata/local/temp"
        ;;
esac
export SUSI_HOME="$home"
export SUSI_HERMETIC_FORBIDDEN="$home"
# Drop a launch-time port offset the same way a real inherited env might set it.
unset SUSI_PORT_OFFSET || true
# Scope it when arguments are given (`-p susi-gawd`), so the rule can be enforced
# per affected crate instead of only as a whole-workspace sweep nobody runs.
runner="${SUSI_HERMETIC_RUNNER:-cargo-test}"
echo "hermetic: setup took $((SECONDS - hermetic_started))s" >&2
tests_started=$SECONDS
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
    # .config/nextest.toml's eight threads are sized for a 4-vCPU CI runner.
    # The local gate runs one thread per logical core: most of the suite is
    # in-process and short, and the process-spawning tests reserve two slots
    # each there (`threads-required`), so at most half the threads run those
    # at once - the CPU headroom their deadlines assume. The root package's
    # 598 tests took 916 s on 8 threads of a 28-core machine (7 188 s of test
    # time: Windows file I/O makes the fixture-copying tests 6x a Linux
    # runner's), most of it waiting on I/O one test at a time.
    if [ -z "${NEXTEST_TEST_THREADS:-}" ]; then
        cores=$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 8)
        [ "$cores" -gt 8 ] 2>/dev/null && export NEXTEST_TEST_THREADS="$cores"
    fi
    # A crate with no tests (most vendor shims) is a pass, as it is under cargo test.
    cargo nextest run --locked --no-fail-fast --no-tests=pass \
        --status-level "${NEXTEST_STATUS_LEVEL:-slow}" \
        --final-status-level "${NEXTEST_FINAL_STATUS_LEVEL:-slow}" \
        --retries "${SUSI_HERMETIC_RETRIES:-2}" "${scope[@]}"
    echo "hermetic: build and tests took $((SECONDS - tests_started))s" >&2
    docs_started=$SECONDS
    # Doc-tests: `-p` on a package with no library target is an error, so name only
    # the selected packages that have one. A selection with no `-p` (`--workspace`)
    # is passed through: cargo skips the bin-only members itself. When `--test`
    # narrows the run to named integration-test binaries, doc-tests are not part
    # of the target set (and `cargo test --doc` rejects `--test`).
    has_test_filter=0
    wanted=()
    prev=""
    for arg in "${scope[@]}"; do
        if [ "$arg" = --test ]; then has_test_filter=1; fi
        if [ "$prev" = -p ]; then wanted+=("$arg"); fi
        prev="$arg"
    done
    if [ "$has_test_filter" -eq 0 ]; then
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
    fi
    echo "hermetic: doc tests took $((SECONDS - docs_started))s" >&2
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
