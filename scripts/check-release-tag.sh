#!/usr/bin/env bash
# A release tag is a claim: "this version, this code". Nothing verified it. The
# release workflow publishes binaries on any `v*.*.*` tag, so a tag on a side
# branch — or one whose name disagrees with the workspace version — would
# publish code that `main` never contained, under a version nobody cut. Tag
# rulesets cannot help: they protect tags that exist, not the truth of a new one.
#
#   scripts/check-release-tag.sh <tag>          e.g. v0.21.0
#
# Two checks, both fatal:
#   1. the tag is `v` + the workspace version in the root `Cargo.toml`
#   2. the tagged commit is an ancestor of `origin/main`
#
# Fail-closed on purpose: if `origin/main` cannot be resolved, a release is not
# verifiable and this refuses rather than guessing.
set -euo pipefail

tag=${1:?usage: check-release-tag.sh <tag>}

# The root package's version, not the first `version = ` anywhere in the file.
version=$(awk '/^\[package\]/{p=1} p && /^version[[:space:]]*=/{gsub(/[",]/, "", $3); print $3; exit}' Cargo.toml)
if [ -z "$version" ]; then
    echo "❌ cannot read the workspace version from Cargo.toml" >&2
    exit 1
fi

if [ "$tag" != "v$version" ]; then
    echo "❌ $tag is not the workspace version: Cargo.toml says $version, so the tag must be v$version" >&2
    exit 1
fi

if ! git rev-parse --verify --quiet "refs/tags/$tag" >/dev/null; then
    echo "❌ no such tag: $tag" >&2
    exit 1
fi

commit=$(git rev-list -n1 "$tag")
if ! git rev-parse --verify --quiet origin/main >/dev/null; then
    echo "❌ origin/main is not available: cannot prove $tag was released from main" >&2
    exit 1
fi
if ! git merge-base --is-ancestor "$commit" origin/main; then
    echo "❌ $tag ($commit) is not an ancestor of origin/main: a release must be code main contains" >&2
    echo "   Tag the merge commit on main (fix forward with a new tag), never a side branch." >&2
    exit 1
fi

echo "✅ $tag is v$version on origin/main ($commit)"
