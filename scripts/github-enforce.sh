#!/usr/bin/env bash
# Make the susi workflow binding on GitHub itself (Mandates 49-51, 54).
#
# Client-side hooks are skipped by `git commit --no-verify`, so the real
# enforcement is a repository ruleset on `main`:
#   * no direct pushes — every change arrives through a pull request,
#   * the branch-push checks must be green (incl. "Workflow Compliance", which
#     rejects commits that do not belong to a claimed or closed task),
#   * no force-push, no deletion, and NO bypass actors (admins included).
# Auto-merge (.github/workflows/auto-merge.yml) still works: it merges through
# the PR API once the required checks pass.
#
#   scripts/github-enforce.sh            print the ruleset and current state
#   scripts/github-enforce.sh --apply    create or update it (needs repo admin)
#
# Roll out by first letting "Workflow Compliance" run green on a few branches:
# a required check that never reports blocks every merge.
set -euo pipefail
repo=${SUSI_REPO:-$(gh repo view --json nameWithOwner --jq .nameWithOwner)}
name="susi-main-workflow"

ruleset=$(cat <<JSON
{
  "name": "$name",
  "target": "branch",
  "enforcement": "active",
  "conditions": { "ref_name": { "include": ["refs/heads/main"], "exclude": [] } },
  "bypass_actors": [],
  "rules": [
    { "type": "deletion" },
    { "type": "non_fast_forward" },
    { "type": "pull_request", "parameters": {
        "required_approving_review_count": 0,
        "dismiss_stale_reviews_on_push": false,
        "require_code_owner_review": false,
        "require_last_push_approval": false,
        "required_review_thread_resolution": false } },
    { "type": "required_status_checks", "parameters": {
        "strict_required_status_checks_policy": false,
        "required_status_checks": [
          { "context": "Workflow Compliance" },
          { "context": "Format Check" },
          { "context": "cargo deny (licenses + advisories)" },
          { "context": "Compile Check (branch pushes)" } ] } }
  ]
}
JSON
)

if [ "${1:-}" != "--apply" ]; then
    echo "ruleset for $repo (dry run; pass --apply to enforce):"
    echo "$ruleset" | jq .
    echo "existing rulesets:"
    gh api "repos/$repo/rulesets" --jq '.[] | "  \(.id) \(.name) [\(.enforcement)]"'
    exit 0
fi

id=$(gh api "repos/$repo/rulesets" --jq ".[] | select(.name==\"$name\") | .id" | head -1)
if [ -n "$id" ]; then
    echo "$ruleset" | gh api -X PUT "repos/$repo/rulesets/$id" --input - >/dev/null
    echo "updated ruleset $id on $repo"
else
    echo "$ruleset" | gh api -X POST "repos/$repo/rulesets" --input - >/dev/null
    echo "created ruleset $name on $repo"
fi
