#!/usr/bin/env bash
# Idempotent GitHub repo settings the automation in
# .github/workflows/auto-merge.yml depends on. Needs a gh session with admin
# on the repo; safe to re-run. Branch protection on main (no force-push, no
# deletion) is kept as-is and asserted here.
set -euo pipefail
repo=${1:-$(gh repo view --json nameWithOwner --jq .nameWithOwner)}
# Let workflows open PRs (auto-merge.yml's open-pr job).
gh api -X PUT "repos/$repo/actions/permissions/workflow" \
    -f default_workflow_permissions=read -F can_approve_pull_request_reviews=true >/dev/null
# Merged branches delete themselves.
gh api -X PATCH "repos/$repo" -F delete_branch_on_merge=true >/dev/null
prot=$(gh api "repos/$repo/branches/main/protection" --jq '[.allow_force_pushes.enabled,.allow_deletions.enabled]|@csv')
[ "$prot" = "false,false" ] || { echo "main protection drifted ($prot): force-push/deletion must be off" >&2; exit 1; }
echo "github settings ok for $repo"
