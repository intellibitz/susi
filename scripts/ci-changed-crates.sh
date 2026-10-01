#!/usr/bin/env bash
# Lists the workspace packages touched by the diff vs the merge base with
# BASE_REF (default: origin/main), one package name per line.
#
# Prints "ALL" when a workspace-wide input changed — the lockfile, workspace
# Cargo.toml, the root build.rs (it compiles .agents/*.json into
# susi-gawd-agents), the root package (src/, tests/), or toolchain/lint
# config — or when detection is impossible (missing/shallow base ref,
# metadata failure). A path under crates/ matching no package is also ALL.
#
# `.agents/tasks/` is exempt: it is the shared task queue, one file per task,
# committed with every task's work. Only `.agents/{identity,roadmap,evidence}
# .json` are compiled (root build.rs and susi-gawd-agents/build.rs), so the
# queue is not a build input. Counting it as one meant every agent commit —
# they all carry a task file — forced `cargo check --workspace` on its branch
# push instead of the bounded affected-crate gate.
#
# Prints nothing when the diff touched no crate (docs/CI-only changes), so
# callers can skip cargo entirely.
set -euo pipefail

BASE="${BASE_REF:-origin/main}"

mb=$(git merge-base "$BASE" HEAD 2>/dev/null) || {
    echo ALL
    exit 0
}
changed=$(git diff --name-only "$mb" HEAD)
[ -n "$changed" ] || {
    echo ALL
    exit 0
}
# The task queue is inert at build time; see the header.
changed=$(grep -v '^\.agents/tasks/' <<<"$changed" || true)

if grep -Eq '^(Cargo\.toml|Cargo\.lock|build\.rs|rust-toolchain(\.toml)?|\.agents/|\.cargo/|rustfmt\.toml|clippy\.toml|deny\.toml|tests/|src/|benches/|examples/)' <<<"$changed"; then
    echo ALL
    exit 0
fi

# name|crate-dir per workspace package.
cargo metadata --no-deps --format-version 1 2>/dev/null |
    jq -r '.packages[] | .name + "|" + (.manifest_path | sub("/Cargo.toml$"; ""))' > /tmp/susi-pkgs.$$ || {
        echo ALL
        exit 0
    }

root=$PWD
declare -A seen=()
while IFS= read -r f; do
    case "$f" in
        crates/*) ;;
        *) continue ;; # top-level non-crate files (docs, .github) need no compile gate
    esac
    best=""
    bestlen=0
    while IFS='|' read -r name dir; do
        case "$root/$f" in
            "$dir/"*)
                if ((${#dir} > bestlen)); then
                    best=$name
                    bestlen=${#dir}
                fi
                ;;
        esac
    done < /tmp/susi-pkgs.$$
    if [ -z "$best" ]; then
        rm -f /tmp/susi-pkgs.$$
        echo ALL
        exit 0
    fi
    seen[$best]=1
done <<<"$changed"
rm -f /tmp/susi-pkgs.$$

for p in "${!seen[@]}"; do
    echo "$p"
done
