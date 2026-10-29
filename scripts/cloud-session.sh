#!/usr/bin/env bash
# Is this clone a cloud session's own worktree?
#
# A cloud session (Claude Code on the web, or any agent harness that clones the
# repository into a disposable container) has no laptop behind it: no shared
# primary checkout to keep parked, no sibling worktrees, no watcher, nothing to
# converge once the container is reclaimed. The clone the harness made, on the
# branch it checked out, IS the worker's worktree (Mandate 49's "own worktree on
# its own branch"), and everything the loop needs — build, test, claim, push —
# runs inside the container. It never asks for the laptop (Remote Control, a
# computer folder, a button to press): see docs/cloud-sessions.md.
#
# The fact is recorded on the clone itself, in its git config, so that every
# consumer — the hooks, the loop scripts and the susi binary — reads one answer,
# and no test fixture inherits it from the process environment:
#
#     susi.cloud = true          this clone is a cloud session's
#     susi.cloudTool = CLAUDE    the agent tool the harness runs (identity prefix)
#
#   scripts/cloud-session.sh            exit 0 when this clone is a cloud session's
#   scripts/cloud-session.sh --detect   exit 0 when the ENVIRONMENT is a cloud
#                                       harness (prints the tool); marks nothing
#   scripts/cloud-session.sh --mark     mark this clone when --detect holds
#                                       (or with --force); exit 0 when marked
#   scripts/cloud-session.sh --tool     print the tool the clone was marked with
#
# SUSI_CLOUD_SESSION=1 says "cloud" for this process without a marker, and
# SUSI_CLOUD_SESSION=0 says "not cloud" whatever the marker says: the explicit
# word of the operator (or a test) beats the recorded fact. Only --detect and
# --mark look at the harness's own variable (CLAUDE_CODE_REMOTE_ENVIRONMENT_TYPE,
# which Claude Code sets in its cloud environments and nowhere else): the
# session-start hook marks the clone it was started for, once, and nothing else
# reads it — a test binary run inside a cloud container inherits that
# environment and must not behave differently for it. `git config --unset
# susi.cloud` takes the mark back.
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
"") is_cloud ;;
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
    echo "cloud session: this clone is the worker's own worktree (tool $tool) — no scripts/susi-worktree.sh, no primary checkout to park, and the laptop is never asked for (docs/cloud-sessions.md)"
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
