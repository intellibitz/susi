#!/usr/bin/env bash
# Prune GitHub Releases to the retention policy:
#   - Keep the rolling `dev` pre-release (never touched here).
#   - Keep the newest KEEP_SEMVER_RELEASES immutable semver releases (default 2).
#   - Delete older *GitHub Release* objects for vX.Y.Z tags.
#   - Never delete git tags (no --cleanup-tag) so history/bisect stay intact.
#
# Usage (CI or local with gh auth):
#   ./scripts/prune-old-github-releases.sh
#   KEEP_SEMVER_RELEASES=2 ./scripts/prune-old-github-releases.sh
set -euo pipefail

KEEP="${KEEP_SEMVER_RELEASES:-2}"
if ! [[ "$KEEP" =~ ^[0-9]+$ ]] || [[ "$KEEP" -lt 1 ]]; then
  echo "KEEP_SEMVER_RELEASES must be a positive integer (got: $KEEP)" >&2
  exit 1
fi

if ! command -v gh >/dev/null 2>&1; then
  echo "gh CLI is required" >&2
  exit 1
fi

# Newest first by publish time (semver string sort is wrong for v0.9 vs v0.10).
mapfile -t tags < <(
  gh release list --limit 100 --exclude-drafts --json tagName,isPrerelease,publishedAt \
    --jq '
      [.[]
        | select(.isPrerelease|not)
        | select(.tagName | test("^v[0-9]+\\.[0-9]+\\.[0-9]+$"))
      ]
      | sort_by(.publishedAt)
      | reverse
      | .[].tagName
    '
)

echo "Semver GitHub releases (newest first): ${tags[*]:-none}"
echo "Retention: keep ${KEEP}, delete the rest (tags preserved)."

if [[ "${#tags[@]}" -le "$KEEP" ]]; then
  echo "Nothing to prune."
  exit 0
fi

for ((i = KEEP; i < ${#tags[@]}; i++)); do
  tag="${tags[$i]}"
  echo "Deleting GitHub Release for ${tag} (keeping git tag)..."
  gh release delete "$tag" --yes
done

echo "Prune complete."
