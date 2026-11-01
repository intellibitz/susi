#!/usr/bin/env bash
# Can the waiting agents start, in parallel, right now?
#
# Usage:
#   scripts/check-swarm-readiness.sh [--agent NAME ...]
#   scripts/check-swarm-readiness.sh --self-test
set -euo pipefail
jq() { command jq "$@" | tr -d '\r'; }

NAME="swarm readiness"
ROSTER=(deepseek cursor claude devin codex antigravity)
HOOKS=(commit-msg pre-commit pre-push workflow-guard)

is_live() {
    local lease=${1:-} now=${2:-0}
    [[ "$lease" =~ ^[0-9]+$ ]] && [[ "$now" =~ ^[0-9]+$ ]] && [ "$lease" -gt "$now" ]
}

attribution() {
    local live_claim=$1 named_by=${2:-}
    if [ "$live_claim" = "true" ]; then
        echo "in-progress"
    elif [ -n "$named_by" ]; then
        echo "named"
    else
        echo "unattributed"
    fi
}

claim_is_here() {
    local claim_branch=$1 lease=$2 branch=$3 now=$4
    [ -n "$branch" ] && [ "$claim_branch" = "$branch" ] && is_live "$lease" "$now"
}

self_test() {
    local failures=0
    check() {
        local label=$1 got=$2 want=$3
        if [ "$got" = "$want" ]; then
            echo "  self-test: $label → $got (expected $want) ok"
        else
            echo "  self-test: $label → $got (expected $want) WRONG"
            failures=$((failures + 1))
        fi
    }
    check "a live claim owns the work" "'$(attribution true "")'" "'in-progress'"
    check "a claim on another branch is not this tree's" "$(claim_is_here "someone-else" 200 "mine" 100 && echo True || echo False)" "False"
    check "a live claim on this branch is this tree's" "$(claim_is_here "mine" 200 "mine" 100 && echo True || echo False)" "True"
    check "an expired claim on this branch is nobody's" "$(claim_is_here "mine" 100 "mine" 200 && echo True || echo False)" "False"
    check "no claim, but a task names it" "'$(attribution false "T-X-1")'" "'named'"
    check "no claim and nothing names it" "'$(attribution false "")'" "'unattributed'"
    check "an expired lease is not a claim" "$(is_live 100 200 && echo True || echo False)" "False"
    check "a future lease is live" "$(is_live 200 100 && echo True || echo False)" "True"
    check "a missing lease is not live" "$(is_live "" 100 && echo True || echo False)" "False"
    if [ "$failures" -ne 0 ]; then
        echo "self-test FAILED: $failures case(s) wrong"
        exit 1
    fi
    echo "PASS: a live claim is the record; only genuinely unattributed work fails"
    exit 0
}

for arg in "$@"; do
    [ "$arg" = "--self-test" ] && self_test
done

roster=("${ROSTER[@]}")
if [ "$#" -gt 0 ]; then
    custom_roster=()
    while [ "$#" -gt 0 ]; do
        if [ "$1" = "--agent" ] && [ "$#" -ge 2 ]; then
            custom_roster+=("$2")
            shift 2
        else
            shift
        fi
    done
    if [ "${#custom_roster[@]}" -gt 0 ]; then
        roster=("${custom_roster[@]}")
    fi
fi

now=$(date +%s)
git fetch --quiet origin 2>/dev/null || true

wt_paths=()
wt_branches=()
wt_tokens=()
cur_path=""
while IFS= read -r line; do
    if [[ "$line" == "worktree "* ]]; then
        cur_path=${line#"worktree "}
    elif [[ "$line" == "branch "* ]] && [ -n "$cur_path" ]; then
        br=${line#"branch "}
        br=${br#refs/heads/}
        tok=$(git -C "$cur_path" config --worktree --get susi.agent 2>/dev/null || true)
        tok=$(printf '%s' "$tok" | tr '[:lower:]' '[:upper:]' | tr -d '[:space:]')
        wt_paths+=("$cur_path")
        wt_branches+=("$br")
        wt_tokens+=("$tok")
        cur_path=""
    fi
done < <(git worktree list --porcelain)

failures=0

# Check shared tokens
for tok in $(printf '%s\n' "${wt_tokens[@]}" | grep -v '^$' | sort | uniq -d); do
    failures=$((failures + 1))
    shared_list=""
    for (( i = 0; i < ${#wt_paths[@]}; i++ )); do
        if [ "${wt_tokens[$i]}" = "$tok" ]; then
            shared_list=${shared_list:+$shared_list, }${wt_paths[$i]}
        fi
    done
    echo "❌ $NAME: $tok is the identity of multiple worktrees ($shared_list) — one token per worker: re-provision the extras with a distinct name"
done

# Check tokenless worktrees
for (( i = 0; i < ${#wt_paths[@]}; i++ )); do
    if [ -z "${wt_tokens[$i]}" ]; then
        failures=$((failures + 1))
        echo "❌ $NAME: ${wt_paths[$i]} carries no susi.agent — it would share the clone-wide token with every other such worktree: git -C ${wt_paths[$i]} config --worktree susi.agent <TOKEN>"
    fi
done

missing_drivers=()
for drv in ledger roadmap; do
    if [ -z "$(git config --get "merge.$drv.driver" 2>/dev/null || true)" ]; then
        missing_drivers+=("$drv")
    fi
done
if [ "${#missing_drivers[@]}" -gt 0 ]; then
    drv_str=$(IFS=", "; echo "${missing_drivers[*]}")
    echo "⚠️  $NAME: merge drivers not installed ($drv_str) — a merge of .agents/evidence.json or .agents/roadmap.json would conflict instead of merging: scripts/setup-dev.sh"
fi

claims_here() {
    local path=$1 branch=$2 ref body cbr clease
    while IFS= read -r ref; do
        [ -n "$ref" ] || continue
        body=$(git -C "$path" cat-file -p "$ref" 2>/dev/null || continue)
        cbr=$(jq -r '.branch // empty' <<<"$body" 2>/dev/null || true)
        clease=$(jq -r '.lease_until_unix // 0' <<<"$body" 2>/dev/null || echo 0)
        if claim_is_here "$cbr" "$clease" "$branch" "$now"; then
            echo "${ref##*/}"
        fi
    done < <(git -C "$path" for-each-ref --format='%(refname)' "refs/claims/*" 2>/dev/null || true)
}

fresh_paths=()
for agent in "${roster[@]}"; do
    want=$(printf '%s' "$agent" | tr '[:lower:]' '[:upper:]')
    best_idx=-1
    best_conv=9
    best_behind=1000000
    for (( i = 0; i < ${#wt_paths[@]}; i++ )); do
        tok=${wt_tokens[$i]}
        [[ "$tok" == "$want"* ]] || continue
        br=${wt_branches[$i]}
        conv=1
        if [[ "$br" =~ ^${agent}-[0-9]{8}-[0-9]{6} ]]; then
            conv=0
        fi
        behind=$(git -C "${wt_paths[$i]}" rev-list --count HEAD..origin/main 2>/dev/null || echo 1000000)
        [[ "$behind" =~ ^[0-9]+$ ]] || behind=1000000
        if [ "$conv" -lt "$best_conv" ] || { [ "$conv" -eq "$best_conv" ] && [ "$behind" -lt "$best_behind" ]; }; then
            best_conv=$conv
            best_behind=$behind
            best_idx=$i
        fi
    done
    if [ "$best_idx" -lt 0 ]; then
        echo "❌ $NAME: $agent: no worktree carries a susi.agent starting $want — scripts/susi-worktree.sh $agent-\$(date +%Y%m%d-%H%M%S)"
        failures=$((failures + 1))
        continue
    fi
    path=${wt_paths[$best_idx]}
    branch=${wt_branches[$best_idx]}
    tok=${wt_tokens[$best_idx]}
    fresh_paths+=("$path")

    problems=()
    steps=()
    behind=$(git -C "$path" rev-list --count HEAD..origin/main 2>/dev/null || echo "")
    if [[ "$behind" =~ ^[0-9]+$ ]]; then
        if [ "$behind" -gt 0 ]; then
            steps+=("$behind commit(s) behind origin/main — git fetch origin && git merge origin/main")
        fi
    else
        steps+=("distance from origin/main unknown — git fetch origin")
    fi

    hooks=$(git -C "$path" config --get core.hooksPath 2>/dev/null || true)
    if [[ "$hooks" == /* ]]; then
        resolved=$hooks
    else
        resolved="$path/$hooks"
    fi
    missing_h=()
    for h in "${HOOKS[@]}"; do
        [ -f "$resolved/$h" ] || missing_h+=("$h")
    done
    if [ -z "$hooks" ]; then
        problems+=("core.hooksPath unset — scripts/setup-dev.sh")
    elif [ "${#missing_h[@]}" -gt 0 ]; then
        mh_str=$(IFS=", "; echo "${missing_h[*]}")
        problems+=("hooks missing or not executable: $mh_str — scripts/setup-dev.sh")
    fi

    if [ -n "$(git -C "$path" status --porcelain 2>/dev/null || true)" ]; then
        held_first=$(claims_here "$path" "$branch" | head -n 1)
        if [ -n "$held_first" ]; then
            steps+=("in progress on $held_first — uncommitted changes are expected while a claim is live")
        else
            problems+=("worktree is dirty — commit or stash before the loop starts")
        fi
    fi

    if [ "${#problems[@]}" -gt 0 ]; then
        failures=$((failures + 1))
        for prob in "${problems[@]}"; do
            echo "❌ $NAME: $agent ($path): $prob"
        done
    else
        echo "✅ $NAME: $agent ready in $path (branch $branch, $tok)"
    fi
    for step in "${steps[@]}"; do
        echo "   → $agent: $step   # the loop's first step"
    done
done

# Compute claimable tasks on origin/main
mapfile -t open_ids < <(git ls-tree --name-only origin/main:.agents/tasks/ 2>/dev/null | sed -n 's/\.json$//p' | sort)
mapfile -t closed_ids < <(git ls-tree --name-only origin/main:.agents/tasks/done/ 2>/dev/null | sed -n 's/\.json$//p' | sort)
closed_lookup=" $(printf '%s ' "${closed_ids[@]}") "
live_claims=" $(git ls-remote origin 'refs/claims/*' 2>/dev/null | awk '{sub(/^refs\/claims\//, "", $2); printf "%s ", $2}') "

ready_list=()
for tid in "${open_ids[@]}"; do
    [ -n "$tid" ] || continue
    case "$live_claims" in *" $tid "*) continue ;; esac
    body=$(git show "origin/main:.agents/tasks/$tid.json" 2>/dev/null || continue)
    size=$(jq -r '.size // "?"' <<<"$body" 2>/dev/null || echo "?")
    blocked=0
    while IFS= read -r dep; do
        [ -n "$dep" ] || continue
        case "$closed_lookup" in
        *" $dep "*) ;;
        *) blocked=1; break ;;
        esac
    done < <(jq -r '.deps // [] | .[]' <<<"$body" 2>/dev/null || true)
    if [ "$blocked" -eq 0 ]; then
        ready_list+=("$tid ($size)")
    fi
done

if [ "${#ready_list[@]}" -eq 0 ]; then
    echo "❌ $NAME: no claimable task — every open task is claimed or dependency-blocked (${#open_ids[@]} open, ${#closed_ids[@]} closed); add one: susi tasks add \"…\" --accept \"cargo test …\""
    failures=$((failures + 1))
else
    r_str=$(printf ', %s' "${ready_list[@]}")
    r_str=${r_str#, }
    echo "✅ $NAME: ${#ready_list[@]} claimable task(s) for ${#roster[@]} agent(s): $r_str"
    if [ "${#ready_list[@]}" -lt "${#roster[@]}" ]; then
        echo "⚠️  $NAME: fewer claimable tasks than agents — the rest will wait rather than collide"
    fi
fi

[ "$failures" -eq 0 ]
