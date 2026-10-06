#!/usr/bin/env bash
# Lists the workspace packages touched by the diff vs the merge base with
# BASE_REF (default: origin/main), one package name per line, sorted.
#
#   ci-changed-crates.sh               the packages whose files the diff touched
#   ci-changed-crates.sh --dependents  those plus every workspace package that
#                                      depends on one of them, transitively
#
# Tests belong to the crate that changed; a compile break does not. A signature
# change in susi-core leaves susi-core's own tests green and breaks whichever of
# its dependents calls it, so the compile and lint gate takes `--dependents`
# (normal, dev and build dependencies all count: a dev-dependency edge is a test
# target that stops compiling) while a gate that runs tests takes the bare list.
#
# Prints "ALL" when a workspace-wide input changed — the lockfile, workspace
# Cargo.toml, the root build.rs (it compiles .agents/*.json into
# susi-gawd-agents), or toolchain/lint config — or when detection is
# impossible (missing/shallow base ref, metadata failure). A path under
# crates/ matching no package is also ALL.
#
# The root package's own trees (src/, tests/, benches/, examples/) map to the
# root package rather than ALL: its test binaries are the gate for a
# tests/-only change, and the other workspace members' tests are not.
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

with_dependents=0
case "${1:-}" in
    "") ;;
    --dependents) with_dependents=1 ;;
    *)
        echo "usage: ci-changed-crates.sh [--dependents]" >&2
        exit 2
        ;;
esac

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

if grep -Eq '^(Cargo\.toml|Cargo\.lock|build\.rs|rust-toolchain(\.toml)?|\.agents/|\.cargo/|rustfmt\.toml|clippy\.toml|deny\.toml)' <<<"$changed"; then
    echo ALL
    exit 0
fi

meta=$(cargo metadata --no-deps --format-version 1 2>/dev/null) || {
    echo ALL
    exit 0
}

# name|crate-dir per workspace package.
pkgs=$(jq -r '.packages[] | .name + "|" + (.manifest_path | sub("/Cargo.toml$"; ""))' <<<"$meta") || {
    echo ALL
    exit 0
}

root=$PWD
rootpkg=$(awk -F'|' -v m="$root" '$2 == m { print $1 }' <<<"$pkgs" | head -1)
declare -A seen=()
while IFS= read -r f; do
    case "$f" in
        crates/*) ;;
        src/*|tests/*|benches/*|examples/*)
            # The root package owns these trees; a change here gates that
            # package's targets, not the whole workspace.
            if [ -n "$rootpkg" ]; then
                seen[$rootpkg]=1
                continue
            fi
            echo ALL
            exit 0
            ;;
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
    done <<<"$pkgs"
    if [ -z "$best" ]; then
        echo ALL
        exit 0
    fi
    seen[$best]=1
done <<<"$changed"

if [ "$with_dependents" = 1 ] && [ "${#seen[@]}" -gt 0 ]; then
    # dependent|dependency, restricted to edges between workspace packages. A
    # dependency's `name` is the package name, never a `package = ` rename.
    edges=$(jq -r '
        [.packages[].name] as $ws
        | .packages[] | .name as $dependent
        | .dependencies[] | select(.name as $d | $ws | index($d))
        | $dependent + "|" + .name' <<<"$meta") || {
        echo ALL
        exit 0
    }
    # Grow the set until no edge leads from it to a package outside it.
    grew=1
    while [ "$grew" = 1 ]; do
        grew=0
        while IFS='|' read -r dependent dependency; do
            [ -n "$dependent" ] || continue
            if [ -n "${seen[$dependency]:-}" ] && [ -z "${seen[$dependent]:-}" ]; then
                seen[$dependent]=1
                grew=1
            fi
        done <<<"$edges"
    done
fi

for p in "${!seen[@]}"; do
    echo "$p"
done | sort
