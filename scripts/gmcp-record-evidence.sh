#!/usr/bin/env bash
# Re-record GMCP evidence against the current commit.
#
#   scripts/gmcp-record-evidence.sh [VC-203-NNN ...]             requirement checks
#   scripts/gmcp-record-evidence.sh --mastery [VC-203-NNN ...]   mastery checks
#
# A VC-203 evidence record (.agents/gmcp/evidence/README.md) binds to the
# commit its check passed on: `tested_commit`, and `tested_artifacts`, the
# sha256 of the committed artifact tree plus the GMCP manifest with only
# package.version omitted (artifact_tree_digest in tests/gmcp_requirements.rs
# builds the same bytes). A mastery test rejects a record whose digest is no
# longer HEAD's, so a change to any of those files stales the vector's
# evidence until both phases run again. Default vectors: every one with
# evidence. Both phases need a clean HEAD, and record only what passed on it.
#
# 1. Without --mastery: run every non-mastery check the newest records name,
#    and only when all pass append one superseding record per requirement.
#    Commit what it appends.
# 2. With --mastery, on that commit: run the vectors' mastery tests (which
#    read the committed evidence of step 1), and only when they all pass
#    append the -M records. Commit those too. A mastery test never reads its
#    own -M record, so this record is exactly what it proved.
set -euo pipefail
# Native Windows jq emits CRLF; keep check names and JSONL portable in Git Bash.
jq() { command jq "$@" | tr -d '\r'; }

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

mastery=0
if [ "${1:-}" = --mastery ]; then
    mastery=1
    shift
fi
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
        for shared in "$tests" "$ledger" .agents/gmcp/sources.json .agents/gmcp/spec \
            .agents/gmcp/fixtures docs/gmcp docs/gmcp-roadmap.md \
            scripts/check-gmcp-ledger.sh crates/susi-gmcp/src crates/susi-gmcp/Cargo.toml; do
            [ -e "$shared" ] && printf '%s\n' "$shared"
        done
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
        is_mastery=0
        case "$name" in gmcp_vc_*) is_mastery=1 ;; esac
        if [ "$is_mastery" = "$mastery" ]; then
            checks+=("$name")
        fi
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
    # Same byte stream as artifact_tree_digest: object IDs except the raw
    # manifest blob, followed by its content with only package.version omitted.
    digest=$({
        git ls-tree -r --full-tree "$head" -- "${paths[@]}" |
            awk -F '\t' '$2 != "crates/susi-gmcp/Cargo.toml"'
        printf 'susi-gmcp manifest without package.version\n'
        git show "$head:crates/susi-gmcp/Cargo.toml" | {
            package=0
            while :; do
                newline=1
                IFS= read -r line || newline=0
                [[ $newline == 0 && -z $line ]] && break
                if [[ $line =~ ^[[:space:]]*\[ ]]; then
                    package=0
                    [[ $line =~ ^[[:space:]]*\[package\][[:space:]]*$ ]] && package=1
                fi
                if ! [[ $package == 1 && $line =~ ^[[:space:]]*version[[:space:]]*= ]]; then
                    printf '%s' "$line"
                    if [[ $newline == 1 ]]; then printf '\n'; fi
                fi
                if [[ $newline == 0 ]]; then break; fi
            done
        }
    } | sha256sum | cut -d' ' -f1)
    newest "$file" | jq -c --argjson mastery "$mastery" \
        --arg ledger_sha "$ledger_sha" --arg who "$who" --arg head "$head" \
        --arg digest "$digest" --argjson now "$now" '
      select((.check | test(" gmcp_vc_[^ ]*$")) == ($mastery == 1))
      | {requirement, check, exit: 0, detail, recorded_unix: $now,
       artifact_sha256: $ledger_sha, identity: $who,
       tested_commit: $head, tested_artifacts: $digest,
       supersedes: {recorded_unix, identity, artifact_sha256}}
    ' >>"$file"
    echo "gmcp-record-evidence: $vector: recorded against $head" >&2
done
