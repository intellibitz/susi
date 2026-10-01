#!/usr/bin/env bash
# One compiler for CI and for every agent's local gate.
#
# CI installs `stable`, so without a pin a toolchain roll reddens `main` with
# no code change at all: 1.99.0 deprecated `Atomic::fetch_update`, and that
# alone failed the clippy job on unchanged code (T-DEEPSEEK-8). The lockfile is
# committed for exactly that reason — determinism — so the compiler is pinned
# too. `rust-toolchain.toml` selects the version; this asserts that the
# toolchain actually resolved here is that one, which matters because the CI
# jobs install `stable` first and the file has to win.
#
#   scripts/check-toolchain-pin.sh
set -uo pipefail

root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
cd "$root"

file=rust-toolchain.toml
[ -f "$file" ] || {
    echo "❌ no $file: this repository's toolchain is unpinned" >&2
    exit 1
}
pinned=$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$file" | head -1)
[ -n "$pinned" ] || {
    echo "❌ $file declares no channel" >&2
    exit 1
}

fail=0
check() { # label, actual, expected
    [ "$2" = "$3" ] || {
        echo "❌ $1 is '$2', not the pinned '$3' (a RUSTUP_TOOLCHAIN override would do this)" >&2
        fail=1
    }
}

check rustc "$(rustc --version 2>/dev/null | awk '{print $2}')" "$pinned"
check cargo "$(cargo --version 2>/dev/null | awk '{print $2}')" "$pinned"
# clippy reports 0.1.<minor> for rustc 1.<minor>.<patch>.
minor=${pinned#1.}
minor=${minor%%.*}
check clippy "$(cargo clippy --version 2>/dev/null | awk '{print $2}')" "0.1.$minor"

if command -v rustup >/dev/null; then
    active=$(rustup show active-toolchain 2>/dev/null | awk '{print $1}')
    case "$active" in
        "$pinned"*) ;;
        *)
            echo "❌ rustup resolved '$active', not $pinned" >&2
            fail=1
            ;;
    esac
fi

if [ "$fail" = 0 ]; then
    echo "✅ toolchain pin: rustc, cargo and clippy are $pinned ($file)"
fi
exit "$fail"
