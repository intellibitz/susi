#!/usr/bin/env bash
# Tag the version a merge into main carries, if it is not tagged yet: merging
# the branch `susi admin release --cut` pushed is what releases it. The merge
# bot (scripts/auto-merge-pr.sh) runs this on every merge it makes and, when a
# tag comes back, dispatches release.yml on it, which builds and publishes.
#
#   scripts/release-tag-merged-bump.sh <sha>
#
# Reads the root Cargo.toml *at <sha>*, not the checkout's. Prints the new tag
# on stdout when it made one, nothing when the version was already tagged;
# everything else goes to stderr.
#
# The cut used to wait up to 30 minutes for its own merge and push the tag
# itself, holding the operator on CI. A tag or merge pushed with GITHUB_TOKEN
# starts no workflow, so the bot that merges is the one that starts the release.
#
# exit 0: tagged (tag on stdout), or nothing to tag (empty stdout)
# exit 1: the tag could not be made or pushed
set -euo pipefail

sha=${1:?usage: release-tag-merged-bump.sh <sha>}

# The root package's version, as scripts/check-release-tag.sh reads it.
version=$(git show "$sha:Cargo.toml" |
    awk '/^\[package\]/{p=1} p && /^version[[:space:]]*=/{gsub(/[",]/, "", $3); print $3; exit}' |
    tr -d '\r')
if [ -z "$version" ]; then
    echo "❌ cannot read the workspace version from $sha:Cargo.toml" >&2
    exit 1
fi
tag="v$version"

git fetch --quiet --tags origin
if existing=$(git rev-list -n1 "refs/tags/$tag" 2>/dev/null); then
    echo "$tag is already tagged ($existing): nothing to release" >&2
    exit 0
fi

git -c user.name="github-actions[bot]" \
    -c user.email="41898282+github-actions[bot]@users.noreply.github.com" \
    tag -a "$tag" -m "Release $tag" "$sha"
git push --quiet origin "refs/tags/$tag" >&2
echo "✅ tagged $tag on $sha" >&2
echo "$tag"
