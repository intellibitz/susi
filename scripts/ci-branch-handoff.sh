#!/usr/bin/env bash
# Fail closed unless this branch will run the required GitHub test check.
set -euo pipefail
# A revision:path is not a Windows PATH list; MSYS must pass it literally.
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'
[ "${SUSI_LOCAL_GATE:-}" != full ] && [ -z "${SUSI_GATE_NOCACHE:-}" ] || exit 1
root=$(git rev-parse --show-toplevel)
cd "$root"
origin=$(git remote get-url origin 2>/dev/null) || exit 1
case "$origin" in
    git@github.com:*|https://github.com/*|ssh://git@github.com/*) ;;
    *) exit 1 ;;
esac
git show origin/main:.github/workflows/test.yml 2>/dev/null |
    grep -F 'SUSI_BRANCH_TEST_HANDOFF_V1' >/dev/null || exit 1
git cat-file -e origin/main:scripts/ci-branch-tests.sh 2>/dev/null || exit 1
base=$(git merge-base origin/main HEAD 2>/dev/null) || exit 1
# A change to CI, the test planner or hooks must use the local gate until the
# new machinery itself has merged. Check committed and uncommitted changes.
paths=(.github/ .githooks/ scripts/ .config/nextest.toml)
git diff --quiet "$base" HEAD -- "${paths[@]}" || exit 1
status=$(git status --porcelain --untracked-files=all -- "${paths[@]}") || exit 1
[ -z "$status" ] || exit 1
