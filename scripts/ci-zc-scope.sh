#!/usr/bin/env bash
# Can the diff from <base> to HEAD change what the Zero-Config Matrix proves?
# Prints `true` or `false`; never fails (anything unresolvable is `true`).
#
#   ci-zc-scope.sh <base-sha>
#
# The matrix runs the root package's installer and lifecycle tests on three
# OS images (tests/zc_platform_installers.rs, tests/vc_201_098_mastery.rs):
# they read install.sh and install.ps1 and exercise the `susi` binary, so the
# verdict can move when the installers, the matrix's own machinery, or any
# crate the root package depends on changed — `ci-changed-crates.sh
# --dependents` folds the latter in, and the root package depends on nearly
# everything. What is left is the merge that reaches no crate and no
# installer: a task record, a doc, CI prose. About half of all merges are
# that, and each one used to start a ~15 min three-OS build (a macOS and a
# Windows runner among them) to re-prove an unchanged binary.
set -uo pipefail
base=${1:?usage: ci-zc-scope.sh <base-sha>}
here=$(cd "$(dirname "$0")" && pwd)
cd "$(git rev-parse --show-toplevel)" || { echo true; exit 0; }

git rev-parse --verify --quiet "$base^{commit}" >/dev/null || { echo true; exit 0; }
# Rename detection off: a file moved out of scripts/ or an installer renamed
# still names the path it left.
changed=$(git -c diff.renames=false diff --no-renames --name-only "$base" HEAD 2>/dev/null) || { echo true; exit 0; }
# The installers, the matrix's own definition and workflow, the scripts the
# job runs, and the vendored patch crates (`[patch.crates-io]` path crates
# linked into the binary) and xtask, which no workspace package maps to.
if grep -Eq '^(install\.sh|install\.ps1|\.github/workflows/zc-matrix\.yml|\.github/zc-matrix/|scripts/|vendor/|xtask/)' <<<"$changed"; then
    echo true
    exit 0
fi
pkgs=$(BASE_REF="$base" "$here/ci-changed-crates.sh" --dependents 2>/dev/null) || { echo true; exit 0; }
if [ -n "$pkgs" ]; then
    echo true
else
    echo false
fi
