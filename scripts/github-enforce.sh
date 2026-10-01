#!/usr/bin/env bash
# Make the susi workflow binding on GitHub itself (Mandates 49-51, 53, 54).
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
#   ... any of the above with --tags              include the release-tag ruleset
#
# Phases (start at 1):
#   1  pull requests only + no force-push + no deletion. Blocks the failure that
#      matters (a direct push to main) and adds no way to get stuck.
#   2  adds the required checks, incl. "Workflow Compliance". Move here once that
#      check has run green on a few weeks of branches: a required check that
#      never reports, or a renamed job, blocks every merge. All four contexts run
#      on branch pushes, which is where a pull request is judged.
# Auto-merge (.github/workflows/auto-merge.yml) is unaffected: it merges through
# the pull-request API once the branch-push run is green.
#
# `--tags` adds a second ruleset over `refs/tags/v*` that blocks moving or
# deleting a release tag. Creation stays open, because the release flow pushes
# its own tag — what this prevents is a published version being silently
# re-pointed at different code, which is the reason to pin versions at all.
# (Mandate 53's "fix forward with a new tag" already forbids the legitimate
# case of rewriting one.) Enforcing the *provenance* of a new tag — that it is on
# main and matches the workspace version — is the release job's own check.
set -euo pipefail

repo=${SUSI_REPO:-$(gh repo view --json nameWithOwner --jq .nameWithOwner)}
name="susi-main-workflow"
tag_name="susi-release-tags"

mode=dry
phase=1
tags=0
while [ $# -gt 0 ]; do
    case "$1" in
    --apply) mode=apply ;;
    --relax) mode=relax ;;
    --status) mode=status ;;
    --tags) tags=1 ;;
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

tag_rules=$(cat <<JSON
[
  { "type": "deletion" },
  { "type": "update" }
]
JSON
)
tag_ruleset=$(jq -n --arg name "$tag_name" --arg enf "$enforcement" --argjson rules "$tag_rules" '{
  name: $name, target: "tag", enforcement: $enf,
  conditions: { ref_name: { include: ["refs/tags/v*"], exclude: [] } },
  bypass_actors: [], rules: $rules }')

# The id of a ruleset by name, or empty.
ruleset_id() {
    gh api "repos/$repo/rulesets" --jq ".[] | select(.name==\"$1\") | .id" | head -1 || true
}

branch_id=$(ruleset_id "$name")
tag_id=""
[ "$tags" = 0 ] || tag_id=$(ruleset_id "$tag_name")

case "$mode" in
dry)
    echo "ruleset for $repo, phase $phase (dry run; --apply to enforce):"
    echo "$ruleset" | jq .
    if [ "$tags" = 1 ]; then
        echo "release-tag ruleset for $repo (dry run; --apply --tags to enforce):"
        echo "$tag_ruleset" | jq .
    fi
    ;;
status)
    report() { # name, id, what it protects
        if [ -z "$2" ]; then
            echo "no $1 ruleset on $repo: $3 is NOT protected by it"
        else
            gh api "repos/$repo/rulesets/$2" --jq '"\(.name) [\(.enforcement)] rules: \([.rules[].type]|join(", "))"'
        fi
    }
    report "$name" "$branch_id" "main"
    [ "$tags" = 0 ] || report "$tag_name" "$tag_id" "release tags"
    ;;
relax)
    if [ -z "$branch_id" ] && { [ "$tags" = 0 ] || [ -z "$tag_id" ]; }; then
        echo "no $name ruleset to relax on $repo" >&2
        exit 1
    fi
    if [ -n "$branch_id" ]; then
        echo "$ruleset" | gh api -X PUT "repos/$repo/rulesets/$branch_id" --input - >/dev/null
        echo "RELAXED: $name is disabled on $repo. Restore with: scripts/github-enforce.sh --apply"
    fi
    if [ "$tags" = 1 ]; then
        if [ -n "$tag_id" ]; then
            echo "$tag_ruleset" | gh api -X PUT "repos/$repo/rulesets/$tag_id" --input - >/dev/null
            echo "RELAXED: $tag_name is disabled on $repo. Restore with: scripts/github-enforce.sh --apply --tags"
        else
            echo "no $tag_name ruleset to relax on $repo" >&2
        fi
    fi
    ;;
apply)
    upsert() { # name, body, id
        if [ -n "$3" ]; then
            echo "$2" | gh api -X PUT "repos/$repo/rulesets/$3" --input - >/dev/null
            echo "updated $1 (phase $phase, active) on $repo"
        else
            echo "$2" | gh api -X POST "repos/$repo/rulesets" --input - >/dev/null
            echo "created $1 (phase $phase, active) on $repo"
        fi
    }
    upsert "$name" "$ruleset" "$branch_id"
    [ "$tags" = 0 ] || upsert "$tag_name" "$tag_ruleset" "$tag_id"
    ;;
esac
