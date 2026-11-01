#!/usr/bin/env bash
# Re-record GMCP evidence against the current commit.
#
#   scripts/gmcp-record-evidence.sh [VC-203-NNN ...]   default: every vector with evidence
#
# A VC-203 evidence record (.agents/gmcp/evidence/README.md) binds to the
# commit its check ran on: `tested_commit`, and `tested_artifacts`, the sha256
# of `git ls-tree -r --full-tree <commit> -- <artifacts>` over the requirement
# tests, the ledger and the vector's existing production paths
# (evidence_artifacts in tests/gmcp_requirements.rs builds the same set). The
# mastery tests reject a record whose digest is no longer HEAD's, so a change
# to any of those files leaves the vector's evidence stale until this runs.
#
# On a clean HEAD this runs every non-mastery check the newest records name,
# and only when all of them pass appends one superseding record per
# requirement with tested_commit=HEAD. A mastery (-M) record is proven by its
# own test, which reads the committed evidence: commit what this appends,
# then run `cargo test --locked --test gmcp_requirements gmcp_vc_`, and revert
# the commit if a mastery test fails.
set -euo pipefail

root=$(git rev-parse --show-toplevel)
cd "$root"
if [ -n "$(git status --porcelain)" ]; then
    echo "gmcp-record-evidence: commit first: a record names the commit its checks ran on" >&2
    exit 1
fi

ledger=.agents/gmcp/requirements-ledger.json
tests=tests/gmcp_requirements.rs
prefix='cargo test --locked --test gmcp_requirements '
head=$(git rev-parse HEAD)
ledger_sha=$(sha256sum "$ledger" | cut -d' ' -f1)
who=${SUSI_AGENT:-$(git config user.name)}
now=$(date +%s)

vectors=("$@")
if [ ${#vectors[@]} -eq 0 ]; then
    mapfile -t vectors < <(find .agents/gmcp/evidence -name 'VC-203-*.jsonl' -printf '%f\n' | sed 's/\.jsonl$//' | LC_ALL=C sort)
fi

# The newest record per requirement: the record no later record of its
# requirement supersedes, the latest recorded_unix among them, file order
# breaking ties (newest_records in tests/gmcp_requirements.rs).
newest() {
    jq -sc '
      [to_entries[] | .value + {_line: .key}]
      | group_by(.requirement)[]
      | . as $chain
      | [ .[] | . as $r
          | select(($chain | any(.supersedes != null
              and .supersedes.recorded_unix == $r.recorded_unix
              and .supersedes.identity == $r.identity
              and .supersedes.artifact_sha256 == $r.artifact_sha256)) | not) ]
      | max_by([.recorded_unix, ._line])
    ' "$1"
}

# The vector's evidence artifacts, one per line (evidence_artifacts).
artifacts() {
    {
        jq -r --arg v "$1" '.requirements[] | select(.trace.vector == $v) | .trace.production_path' "$ledger" |
            while IFS= read -r path; do
                case "/$path/" in */../* | */./* | //*) continue ;; esac
                [ -e "$path" ] && printf '%s\n' "$path"
            done
        printf '%s\n' "$ledger" "$tests"
    } | LC_ALL=C sort -u
}

checks=()
for vector in "${vectors[@]}"; do
    file=.agents/gmcp/evidence/$vector.jsonl
    [ -f "$file" ] || { echo "gmcp-record-evidence: no evidence file $file" >&2; exit 1; }
    while IFS= read -r check; do
        name=${check#"$prefix"}
        if [ "$name" = "$check" ] || [ -z "$name" ] || [[ $name == *[[:space:]]* ]]; then
            echo "gmcp-record-evidence: $vector: not a gmcp_requirements test: $check" >&2
            exit 1
        fi
        grep -q "^fn $name()" "$tests" || { echo "gmcp-record-evidence: $vector: no test $name" >&2; exit 1; }
        case "$name" in gmcp_vc_*) ;; *) checks+=("$name") ;; esac
    done < <(newest "$file" | jq -r '.check')
done

if [ ${#checks[@]} -gt 0 ]; then
    echo "gmcp-record-evidence: running ${#checks[@]} checks on $head" >&2
    out=$(cargo test --locked --test gmcp_requirements -- --exact "${checks[@]}" 2>&1) || {
        printf '%s\n' "$out" >&2
        echo "gmcp-record-evidence: a check failed: nothing recorded" >&2
        exit 1
    }
    grep -q "test result: ok. ${#checks[@]} passed" <<<"$out" || {
        printf '%s\n' "$out" >&2
        echo "gmcp-record-evidence: expected ${#checks[@]} tests to run and pass: nothing recorded" >&2
        exit 1
    }
fi

for vector in "${vectors[@]}"; do
    file=.agents/gmcp/evidence/$vector.jsonl
    mapfile -t paths < <(artifacts "$vector")
    digest=$(git ls-tree -r --full-tree "$head" -- "${paths[@]}" | sha256sum | cut -d' ' -f1)
    newest "$file" | jq -c \
        --arg ledger_sha "$ledger_sha" --arg who "$who" --arg head "$head" \
        --arg digest "$digest" --argjson now "$now" '
      {requirement, check, exit: 0, detail, recorded_unix: $now,
       artifact_sha256: $ledger_sha, identity: $who,
       tested_commit: $head, tested_artifacts: $digest,
       supersedes: {recorded_unix, identity, artifact_sha256}}
    ' >>"$file"
    echo "gmcp-record-evidence: $vector: recorded against $head" >&2
done
