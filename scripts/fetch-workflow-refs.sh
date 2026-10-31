#!/usr/bin/env bash
# Fetch the branch tips and SUSI task refs used by the push compliance gate in
# one network round-trip. The caller then sets SUSI_WORKFLOW_REFS_FETCHED=1 so
# check-workflow-compliance.sh reads this exact snapshot without fetching again.
#
# The push guard makes this same fetch itself, in Rust (`susi workflow
# fetch-refs`, crates/susi-gawd/src/admin/guard.rs), with no shell between: this
# script is CI's copy of it, for the compliance job, which builds no susi.
set -euo pipefail
[ ! -f "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh" ] || . "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh"

remote=${1:-origin}
git fetch --quiet --prune "$remote" \
    "+refs/heads/*:refs/remotes/$remote/*" \
    '+refs/claims/*:refs/claims/*' \
    '+refs/closed/*:refs/closed/*'
