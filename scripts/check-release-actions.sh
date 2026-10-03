#!/usr/bin/env bash
# Every action a workflow runs is a version like any other, and a version that
# does not exist kills the job at "Set up job" before any step runs. That is how
# a bump to `softprops/action-gh-release@v4` (245ad77d) took down every release
# build for a day: the tag was pushed, the binaries were never published, and
# the failure only appeared at the slowest possible moment — a release. Resolve
# the references here instead, on the push that changes them.
#
#   scripts/check-release-actions.sh    # 0 = every reference resolves
#
# Offline, or without `gh`, it says so and passes: an unreachable API is not
# evidence that a pin is wrong.
set -uo pipefail
root=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$root" || exit 0
command -v gh >/dev/null 2>&1 || {
    echo "⏭️  workflow actions: no gh on PATH; cannot resolve references"
    exit 0
}

refs=$(grep -rhoE '^[[:space:]]*uses:[[:space:]]*[^[:space:]#]+' .github/workflows/*.yml 2>/dev/null |
    sed -E 's/.*uses:[[:space:]]*//; s/^["'\'']//; s/["'\'']$//' |
    grep '@' | grep -v '^\./' | sort -u) || true
if [ -z "$refs" ]; then
    echo "✅ workflow actions: no external action references"
    exit 0
fi

# A reference is `owner/repo[/subdir]@tag|branch|sha`; the repository is the
# first two path segments, and the pin may be any of the three.
resolves() {
    local repo=$1 pin=$2
    gh api "repos/$repo/git/ref/tags/$pin" >/dev/null 2>&1 && return 0
    gh api "repos/$repo/branches/$pin" >/dev/null 2>&1 && return 0
    gh api "repos/$repo/commits/$pin" >/dev/null 2>&1 && return 0
    return 1
}

failed=0
while IFS= read -r ref; do
    [ -n "$ref" ] || continue
    case "$ref" in docker://*) continue ;; esac
    repo=$(printf '%s' "${ref%@*}" | cut -d/ -f1,2)
    pin=${ref##*@}
    case "$repo" in */*) ;; *) continue ;; esac
    if resolves "$repo" "$pin"; then continue; fi
    if ! gh api "repos/$repo" >/dev/null 2>&1; then
        echo "❌ workflow actions: $repo does not exist (pinned as $ref)" >&2
    else
        echo "❌ workflow actions: $repo has no '$pin' — $ref cannot resolve, so every job using it dies at setup" >&2
    fi
    failed=1
done <<<"$refs"

[ "$failed" = 0 ] && echo "✅ workflow actions: every referenced action version resolves"
exit "$failed"
