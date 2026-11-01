#!/usr/bin/env bash
# Lists the workspace packages touched by the diff vs the merge base with
# BASE_REF (default: origin/main), one package name per line, sorted.
#
#   ci-changed-crates.sh               the packages whose files the diff touched
#   ci-changed-crates.sh --dependents  those plus every workspace package that
#                                      depends on one of them, transitively
#   ci-changed-crates.sh --plan        test:<package>, lint:<package>,
#                                      root-package:<name>, root-test:<target>
#                                      for one complete local gate selection
#
# Tests belong to the crate that changed; a compile break does not. A signature
# change in susi-core leaves susi-core's own tests green and breaks whichever of
# its dependents calls it, so the compile and lint gate takes `--dependents`
# (normal, dev and build dependencies all count: a dev-dependency edge is a test
# target that stops compiling) while a gate that runs tests takes the bare list.
#
# Prints "ALL" when a workspace-wide input changed — external lockfile
# resolution, workspace Cargo.toml, the root build.rs (it compiles .agents/*.json into
# susi-gawd-agents), toolchain/lint config, a vendored patch crate under
# vendor/ — or when detection is impossible (missing/shallow base
# ref, metadata failure). A path under crates/ matching no package is also ALL.
#
# The root package's own trees (src/, tests/, benches/, examples/) map to the
# root package rather than ALL: its test binaries are the gate for a
# tests/-only change, and the other workspace members' tests are not. Removing
# a top-level integration-test file removes a test target but changes no code
# to compile, so a moved test does not pull the entire root app into the gate.
#
# `.agents/tasks/` is exempt: it is the shared task queue, one file per task,
# committed with every task's work. Only `.agents/{identity,roadmap,evidence}
# .json` are compiled (root build.rs and susi-gawd-agents/build.rs), so the
# queue is not a build input. Counting it as one meant every agent commit —
# they all carry a task file — forced `cargo check --workspace` on its branch
# push instead of the bounded affected-crate gate.
#
# config/ and deploy/ are data the crates compile in (`include_str!`, thirty-odd
# sites, the harness price table among them) or read at run time by a path
# their sources spell out. A change there is a change to the packages whose
# Rust sources name the file, or a directory above it (two components or more:
# `config` alone names nothing). Skipping them as "no crate" let a change to
# deploy/harness/prices.json through the local gate, the branch Compile Check
# and main's affected-crate run with no test run at all, while a broken table
# left the harness pricing nothing. A file there that no source names is ALL
# (a crate may still reach it by a path assembled at run time), unless the
# diff deleted it.
#
# Prints nothing when the diff touched no crate (docs/CI-only changes), so
# callers can skip cargo entirely.
set -euo pipefail
[ ! -f "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh" ] || . "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh"

with_dependents=0
root_test_targets=0
gate_plan=0
case "${1:-}" in
    "") ;;
    --dependents) with_dependents=1 ;;
    --root-test-targets) root_test_targets=1 ;;
    --plan) gate_plan=1; with_dependents=1; root_test_targets=1 ;;
    *)
        echo "usage: ci-changed-crates.sh [--dependents|--root-test-targets|--plan]" >&2
        exit 2
        ;;
esac

BASE="${BASE_REF:-origin/main}"

mb=$(git merge-base "$BASE" HEAD 2>/dev/null) || {
    echo ALL
    exit 0
}
# Both sides of a rename: with rename detection (git's default) a renamed
# config/ file showed only its new path, and a crate that still compiles in
# the old one was never gated. Paths read as git prints them whatever the
# user's config says (relative to a subdirectory, quoted, colored).
changed=$(git -c diff.relative=false -c core.quotePath=false -c color.ui=never \
    diff --no-renames --no-color --name-only "$mb" HEAD)
# A branch whose content is its base's has nothing of its own to prove: the
# base's suite already proved these bytes. This used to print ALL, so a branch
# holding only merges of main ran the whole workspace suite, and publishing
# a task's done record (`susi workflow finish` gates before it commits the
# record) took hours instead of seconds.
[ -n "$changed" ] || exit 0
# The task queue is inert at build time; see the header.
changed=$(grep -v '^\.agents/tasks/' <<<"$changed" || true)

# A top-level root integration-test-only diff can be linted by target. This is
# deliberately a narrow opt-in result: any production/build input, deleted or
# nested test target, or unrecognized path prints nothing so callers retain
# their conservative all-targets path. Workflow/docs/task-only paths are inert
# to Rust target compilation and may accompany a focused test edit.
if [ "$root_test_targets" = 1 ]; then
    repo_root=$(git rev-parse --show-toplevel)
    target_tests=()
    target_uncertain=0
    while IFS= read -r f; do
        case "$f" in
            tests/*.rs)
                name=${f#tests/}
                case "$name" in */*) target_uncertain=1 ;; *)
                    if [ -f "$repo_root/$f" ]; then
                        target_tests+=("${name%.rs}")
                    else
                        target_uncertain=1
                    fi
                    ;;
                esac
                ;;
            .agents/tasks/*|scripts/*|.github/*|.config/*|docs/*|*.md) ;;
            src/*|benches/*|examples/*|config/*|deploy/*) target_uncertain=1; break ;;
            *)
                # The gate can focus root tests alongside edits in other
                # packages; their own tests and lint closure are listed below.
                [ "$gate_plan" = 1 ] || { target_uncertain=1; break; }
                ;;
        esac
    done <<<"$changed"
    if [ "$gate_plan" != 1 ] && [ "$target_uncertain" = 0 ] && [ "${#target_tests[@]}" -gt 0 ]; then
        printf '%s\n' "${target_tests[@]}" | sort -u
    fi
    [ "$gate_plan" = 1 ] || exit 0
fi

# These root integration tests read the release implementation as source text.
# A crate-only gate otherwise misses them even though a release change can make
# the tagged suite fail. If root production inputs also changed, use ALL rather
# than incorrectly narrowing the root package to one integration target.
release_root_test=0
if [ "$gate_plan" = 1 ] && grep -Eq '^crates/susi-gawd/src/admin/(release|version)\.rs$' <<<"$changed"; then
    if [ "$target_uncertain" != 0 ] || [ ! -f "$repo_root/tests/release_workflow.rs" ]; then
        echo ALL
        exit 0
    fi
    target_tests+=(release_workflow)
    release_root_test=1
fi

# A test-only diff cannot break the packages that consume this crate: the
# branch gate runs clippy with --all-targets for the owning package, which
# compiles its integration tests and dev-dependencies. Workflow, hook, task
# queue, documentation, and script paths may accompany the test edit without changing
# that conclusion. Keep dependent closure for any production/build/config
# input, where an API or generated output can break a caller. Determine this
# before config/deploy paths are mapped to the Rust sources that read them.
test_only=1
while IFS= read -r f; do
    case "$f" in
        crates/*/tests/*|xtask/tests/*|tests/*.rs|.agents/tasks/*|scripts/*|.github/*|.config/*|.githooks/*|docs/*|*.md) ;;
        *) test_only=0; break ;;
    esac
done <<<"$changed"

# vendor/ holds the `[patch.crates-io]` path crates, compiled into nearly
# every workspace crate. Workspace members outside `crates/` (currently
# `xtask/`) still map through Cargo metadata below, so they get their own
# bounded gate instead of forcing a workspace-wide compile.
if grep -Eq '^(Cargo\.toml|build\.rs|rust-toolchain(\.toml)?|\.agents/|\.cargo/|rustfmt\.toml|clippy\.toml|deny\.toml|vendor/)' <<<"$changed"; then
    echo ALL
    exit 0
fi

# A lockfile diff that only changes path packages (for example, a local crate
# adds a workspace dependency) is already scoped by the changed Cargo.toml
# below. Keep the broad gate for any registry/git package or lock format change:
# those can alter the resolved build graph for every workspace member.
if grep -qx 'Cargo.lock' <<<"$changed"; then
    base_lock=$(git show "$BASE:Cargo.lock" 2>/dev/null) || {
        echo ALL
        exit 0
    }
    external_lock_packages() {
        awk '
            function flush() {
                if (has_package && has_source) print package
            }
            /^version = / {
                if (lock_version == "") lock_version = $0
            }
            /^\[\[package\]\]$/ {
                flush()
                package = $0
                has_package = 1
                has_source = 0
                next
            }
            {
                if ($0 ~ /^source = /) has_source = 1
                if (has_package) package = package "\n" $0
            }
            END {
                flush()
                print lock_version
            }
        '
    }
    base_external=$(external_lock_packages <<<"$base_lock")
    current_external=$(external_lock_packages <Cargo.lock)
    if [ "$base_external" != "$current_external" ]; then
        echo ALL
        exit 0
    fi
    changed=$(grep -vx 'Cargo.lock' <<<"$changed" || true)
    # A lockfile-only diff has no changed package manifest to scope it to.
    [ -n "$changed" ] || {
        echo ALL
        exit 0
    }
fi

if [ -n "${SUSI_CARGO_METADATA_FILE:-}" ]; then
    meta=$(cat "$SUSI_CARGO_METADATA_FILE" 2>/dev/null) || {
        echo ALL
        exit 0
    }
else
    meta=$(cargo metadata --no-deps --format-version 1 2>/dev/null) || {
        echo ALL
        exit 0
    }
fi
if [ -z "$meta" ]; then
    echo ALL
    exit 0
fi

# name|crate-dir per workspace package.
pkgs=$(jq -r '.packages[] | .name + "|" + (.manifest_path | gsub("\\\\"; "/") | sub("/Cargo.toml$"; ""))' <<<"$meta") || {
    echo ALL
    exit 0
}

root=$(git rev-parse --show-toplevel)
rootpkg=$(awk -F'|' -v m="$root" '$2 == m { print $1 }' <<<"$pkgs" | head -1)

# The Rust sources that name each changed data path (or a directory above it),
# one per line; fails when some path that still exists is named by none (a
# deleted one no source names any more needs nothing). One `git grep` for all
# of them: `-o` prints which spelling each source holds.
data_readers() {
    local data patterns p
    local -a args=()
    data=$(cat)
    patterns=$(awk -F/ '{ p = $1; for (i = 2; i <= NF; i++) { p = p "/" $i; print p } }' <<<"$data" | sort -u)
    while IFS= read -r p; do
        [ -n "$p" ] && args+=(-e "$p")
    done <<<"$patterns"
    {
        while IFS= read -r p; do
            if [ -e "$root/$p" ]; then echo "D $p"; else echo "G $p"; fi
        done <<<"$data"
        # "<path>:<match>" whatever the user's config says: grep.lineNumber
        # or grep.column put "<n>:" before the match, color wrapped both.
        git -C "$root" -c grep.lineNumber=false -c grep.column=false -c grep.fullName=false \
            -c color.grep=never -c color.ui=never -c core.quotePath=false \
            grep -I -o -F "${args[@]}" -- '*.rs' 2>/dev/null | sed 's/^/H /' || true
    } | awk '
        /^[DG] / { d[++n] = substr($0, 3); gone[n] = ($1 == "G"); next }
        /^H / {
            s = substr($0, 3); i = index(s, ":"); if (!i) next
            f = substr(s, 1, i - 1); t = substr(s, i + 1)
            if (!((t, f) in pair)) { pair[t, f] = 1; by[t] = by[t] f "\n" }
            next
        }
        END {
            for (k = 1; k <= n; k++) {
                q = d[k]; named = 0
                while (index(q, "/")) {
                    if (q in by) { named = 1; out = out by[q] }
                    sub(/\/[^\/]*$/, "", q)
                }
                if (!named && !gone[k]) exit 1
            }
            printf "%s", out
        }'
}

# Data paths stand for the sources that read them; see the header.
data=$(grep -E '^(config|deploy)/' <<<"$changed" || true)
if [ -n "$data" ]; then
    readers=$(data_readers <<<"$data") || {
        echo ALL
        exit 0
    }
    changed=$(
        grep -vE '^(config|deploy)/' <<<"$changed" || true
        printf '%s\n' "$readers"
    )
fi
declare -A seen=()
while IFS= read -r f; do
    case "$f" in
        crates/*|xtask/*) ;;
        src/*|tests/*|benches/*|examples/*)
            if [[ "$f" == tests/*.rs && ! -f "$root/$f" ]]; then
                continue
            fi
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
        # A workspace-relative package path must resolve to that package or a
        # nested member, never the enclosing root package. Otherwise an
        # unlisted crates/ or xtask/ path silently shrinks to the root target
        # instead of failing open to ALL.
        if [ "$dir" = "$root" ] && [[ "$f" == crates/* || "$f" == xtask/* ]]; then
            continue
        fi
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

if [ "$release_root_test" = 1 ]; then
    [ -n "$rootpkg" ] || { echo ALL; exit 0; }
    seen[$rootpkg]=1
fi

test_packages=("${!seen[@]}")
if [ "$with_dependents" = 1 ] && [ "$test_only" = 0 ] && [ "${#seen[@]}" -gt 0 ]; then
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

if [ "$gate_plan" = 1 ]; then
    # Resolve the local gate from one diff and one metadata snapshot. Keep test
    # owners before growing the lint closure, and emit only after all reads pass.
    {
        printf 'root-package:%s\n' "$rootpkg"
        for p in "${test_packages[@]}"; do printf 'test:%s\n' "$p"; done
        for p in "${!seen[@]}"; do printf 'lint:%s\n' "$p"; done
        if [ "$target_uncertain" = 0 ]; then
            for p in "${target_tests[@]}"; do printf 'root-test:%s\n' "$p"; done
        fi
    } | sort -u
else
    for p in "${!seen[@]}"; do echo "$p"; done | sort
fi
