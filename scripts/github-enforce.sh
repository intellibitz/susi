#!/usr/bin/env bash
# Make the susi workflow binding on GitHub itself (Mandates 49-51, 54).
#
# Client-side hooks are skipped by `git commit --no-verify`, and every agent
# pushes with the admin's own SSH key, so the real enforcement is a repository
# ruleset on `main` — with NO bypass actor (a bypass for the admin would be a
# bypass for every agent). The escape hatch is deliberate and audited instead:
# `--relax` switches the ruleset off, `--apply` switches it back on; each is a
# settings change GitHub records.
#
#   scripts/github-enforce.sh                     print the ruleset (dry run)
#   scripts/github-enforce.sh --apply [--phase N] create or update it (repo admin)
#   scripts/github-enforce.sh --relax             disable it (emergency; re-apply after)
#   scripts/github-enforce.sh --status            show enforcement and rules
#
# Phases (start at 1):
#   1  pull requests only + no force-push + no deletion. Blocks the failure that
#      matters (a direct push to main) and adds no way to get stuck.
#   2  adds the required checks, incl. "Workflow Compliance". Move here once that
#      check has run green on a few weeks of branches: a required check that
#      never reports, or a renamed job, blocks every merge.
# Auto-merge (.github/workflows/auto-merge.yml) is unaffected: it merges through
# the pull-request API once the branch-push run is green.
set -euo pipefail
repo=${SUSI_REPO:-$(gh repo view --json nameWithOwner --jq .nameWithOwner)}
name="susi-main-workflow"

mode=dry
phase=1
while [ $# -gt 0 ]; do
    case "$1" in
    --apply) mode=apply ;;
    --relax) mode=relax ;;
    --status) mode=status ;;
    --phase) phase=${2:?--phase needs 1 or 2}; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done
case "$phase" in 1 | 2) ;; *) echo "phase must be 1 or 2" >&2; exit 2 ;; esac

base_rules=$(cat <<JSON
[
  { "type": "deletion" },
  { "type": "non_fast_forward" },
  { "type": "pull_request", "parameters": {
      "required_approving_review_count": 0,
      "dismiss_stale_reviews_on_push": false,
      "require_code_owner_review": false,
      "require_last_push_approval": false,
      "required_review_thread_resolution": false } }
]
JSON
)
check_rule=$(cat <<JSON
{ "type": "required_status_checks", "parameters": {
    "strict_required_status_checks_policy": false,
    "required_status_checks": [
      { "context": "Workflow Compliance" },
      { "context": "Format Check" },
      { "context": "cargo deny (licenses + advisories)" },
      { "context": "Compile Check (branch pushes)" } ] } }
JSON
)
rules=$base_rules
[ "$phase" = 2 ] && rules=$(jq -c --argjson c "$check_rule" '. + [$c]' <<<"$base_rules")

enforcement=active
[ "$mode" = relax ] && enforcement=disabled
ruleset=$(jq -n --arg name "$name" --arg enf "$enforcement" --argjson rules "$rules" '{
  name: $name, target: "branch", enforcement: $enf,
  conditions: { ref_name: { include: ["refs/heads/main"], exclude: [] } },
  bypass_actors: [], rules: $rules }')

id=$(gh api "repos/$repo/rulesets" --jq ".[] | select(.name==\"$name\") | .id" | head -1 || true)

case "$mode" in
dry)
    echo "ruleset for $repo, phase $phase (dry run; --apply to enforce):"
    echo "$ruleset" | jq .
    ;;
status)
    if [ -z "$id" ]; then echo "no $name ruleset on $repo: main is NOT protected by it"; exit 0; fi
    gh api "repos/$repo/rulesets/$id" --jq '"\(.name) [\(.enforcement)] rules: \([.rules[].type]|join(", "))"'
    ;;
relax)
    [ -n "$id" ] || { echo "no $name ruleset to relax on $repo" >&2; exit 1; }
    echo "$ruleset" | gh api -X PUT "repos/$repo/rulesets/$id" --input - >/dev/null
    echo "RELAXED: $name is disabled on $repo. Restore with: scripts/github-enforce.sh --apply"
    ;;
apply)
    if [ -n "$id" ]; then
        echo "$ruleset" | gh api -X PUT "repos/$repo/rulesets/$id" --input - >/dev/null
        echo "updated $name (phase $phase, active) on $repo"
    else
        echo "$ruleset" | gh api -X POST "repos/$repo/rulesets" --input - >/dev/null
        echo "created $name (phase $phase, active) on $repo"
    fi
    ;;
esac
