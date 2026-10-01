#!/usr/bin/env bash
# A `[[test]] harness = false` target is invisible to cargo-nextest, which is
# what CI runs on main. Nextest discovers tests by executing every test binary;
# for a binary that is not a libtest harness it reports no result and marks the
# test **skipped** — with the guard under test deliberately broken, nextest
# still said "10 passed, 1 skipped" and exited 0. So such a test can never fail
# a shard where it matters, and its discovery pass can fail the whole shard
# instead: that is how `susi-paths::cli_output_contract` broke main's
# foundation shard after PR #185, twice, because branch pushes skip the nextest
# shards and merged it green.
#
# A test that must own `main` — spawning a process, closing a pipe, asserting
# process-level behaviour — belongs in a bin that an ordinary `#[test]` spawns:
# see `crates/susi-paths/src/bin/stdio_guard_probe.rs`, which nextest runs and
# reports (11 tests, 0 skipped) and which fails the shard when it regresses.
#
# Benches are exempt: nextest does not run bench targets at all.
#
#   scripts/check-test-harness-contract.sh
set -uo pipefail

root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
cd "$root"

found=0
while IFS= read -r manifest; do
    # Only a `harness = false` inside a `[[test]]` section opts a *test* out.
    while IFS= read -r hit; do
        [ -n "$hit" ] || continue
        echo "❌ $hit sets harness = false" >&2
        found=$((found + 1))
    done < <(awk '
        /^\[\[test\]\]/ { in_test = 1; next }
        /^\[\[/          { in_test = 0 }
        in_test && /^[[:space:]]*harness[[:space:]]*=[[:space:]]*false/ {
            print FILENAME ":" FNR
        }
    ' "$manifest")
done < <(find . -name Cargo.toml \
    -not -path './target/*' -not -path './.git/*' -not -path '*/.claude/*' 2>/dev/null | sort)

if [ "$found" -gt 0 ]; then
    echo "" >&2
    echo "   nextest reports no result for a non-libtest test binary, so this test" >&2
    echo "   is skipped on main and can never fail a shard. Move the process-level" >&2
    echo "   part into a bin and assert on it from a normal #[test]." >&2
    exit 1
fi

echo "✅ test harness contract: every [[test]] target uses the harness"
