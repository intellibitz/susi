#!/usr/bin/env bash
# Identify cloud agent harnesses for metadata/telemetry ONLY — NEVER exempt primary checkouts.
#
# Every cloud/container agent MUST create its own isolated linked worktree
# (`scripts/susi-worktree.sh` / `worktree_isolation`) and MUST read/write the
# Susi Native Board (`SUSI_BOARD_URL` / `SUSI_BOARD_REMOTE_URL`, `susi board`).
# No harness flag (`SUSI_CLOUD_SESSION`, `susi.cloud`) ever bypasses linked
# worktrees, task/board guards, or native board collaboration (docs/cloud-sessions.md).
#
#   scripts/cloud-session.sh            exits 1 (primary-checkout exemption removed)
#   scripts/cloud-session.sh --detect   exit 0 when the ENVIRONMENT is a cloud
#                                       harness (prints the tool); marks nothing
#   scripts/cloud-session.sh --mark     record susi.cloud metadata when --detect holds
#                                       (or with --force)
#   scripts/cloud-session.sh --tool     print the tool the clone was marked with
set -uo pipefail

usage() { sed -n '2,35p' "$0" | sed 's/^# \{0,1\}//'; }

# The operator's word: 0 = cloud, 1 = not cloud, 2 = nothing said.
explicit() {
    case "$(printf '%s' "${SUSI_CLOUD_SESSION:-}" | tr '[:upper:]' '[:lower:]')" in
    1 | true | yes | on) return 0 ;;
    0 | false | no | off) return 1 ;;
    *) return 2 ;;
    esac
}

marked() { [ "$(git config --get --bool susi.cloud 2>/dev/null)" = true ]; }

is_cloud() {
    explicit
    case $? in
    0) return 0 ;;
    1) return 1 ;;
    esac
    marked
}

# What the environment says the tool is, lower-case; non-zero when it says nothing.
harness_tool() {
    if [ -n "${CLAUDE_CODE_REMOTE_ENVIRONMENT_TYPE:-}" ]; then
        echo claude
        return 0
    fi
    if explicit; then
        # An explicit cloud session may name its tool; "agent" otherwise.
        printf '%s\n' "${SUSI_CLOUD_TOOL:-agent}"
        return 0
    fi
    return 1
}

token() { tr '[:lower:]' '[:upper:]' | tr -cd 'A-Z0-9'; }

case "${1:-}" in
"")
    # Cloud sessions MUST use an isolated linked worktree and never exempt primary checkouts.
    exit 1
    ;;
--detect) harness_tool ;;
--mark)
    force=0
    [ "${2:-}" = --force ] && force=1
    if ! tool=$(harness_tool); then
        [ "$force" = 1 ] || exit 1
        tool=${SUSI_CLOUD_TOOL:-agent}
    fi
    git rev-parse --git-dir >/dev/null 2>&1 || {
        echo "cloud-session: not inside a git repository" >&2
        exit 1
    }
    tool=$(printf '%s' "$tool" | token)
    [ -n "$tool" ] || tool=AGENT
    git config susi.cloud true && git config susi.cloudTool "$tool" || exit 1
    echo "cloud session marked (tool $tool) — isolated linked worktree (scripts/susi-worktree.sh) + Susi Native Board mandatory (docs/cloud-sessions.md)"
    ;;
--tool)
    t=$(git config --get susi.cloudTool 2>/dev/null)
    [ -n "$t" ] || t=$(harness_tool) || exit 1
    printf '%s\n' "$(printf '%s' "$t" | token)"
    ;;
-h | --help) usage ;;
*)
    echo "usage: cloud-session.sh [--detect | --mark [--force] | --tool]" >&2
    exit 2
    ;;
esac
