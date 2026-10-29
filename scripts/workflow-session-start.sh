#!/usr/bin/env bash
# Session-start hook for agent tools (Claude Code SessionStart, or any wrapper):
# run the workflow check and print the result into the session, so the agent
# learns the rules and what to fix before it edits. It never blocks a session.
cd "$(git rev-parse --show-toplevel 2>/dev/null || pwd)" || exit 0

# A cloud session (Claude Code on the web) works in the clone its harness made,
# which has no laptop behind it: no primary checkout to park, no sibling
# worktrees, and no way — nor any need — to reach the operator's machine. Mark
# that clone once, when this hook runs for it (CLAUDE_PROJECT_DIR is the project
# the hook was started for; a fixture repository a test runs this script in is
# never marked), and the hooks, the loop scripts and `susi workflow check` all
# read the mark (scripts/cloud-session.sh, docs/cloud-sessions.md).
cloud=0
if [ -x scripts/cloud-session.sh ]; then
    if [ -n "${CLAUDE_PROJECT_DIR:-}" ] && [ "$(cd "$CLAUDE_PROJECT_DIR" 2>/dev/null && pwd -P)" = "$(pwd -P)" ]; then
        scripts/cloud-session.sh --mark 2>/dev/null || true
    fi
    scripts/cloud-session.sh && cloud=1
fi

if [ "$cloud" = 1 ]; then
    # The ONNX Runtime ort-sys downloads at build time comes from a CDN the cloud
    # proxy refuses; the same release on GitHub is reachable. Idempotent.
    if [ "${SUSI_CLOUD_ONNXRUNTIME:-1}" != 0 ] && [ -x scripts/cloud-onnxruntime.sh ]; then
        scripts/cloud-onnxruntime.sh 2>&1 | sed 's/^/  /'
    fi
    # The harness clones with limited depth; the hooks and the compliance
    # script walk the whole history (`fatal: bad revision '^…^'` otherwise).
    if [ "$(git rev-parse --is-shallow-repository 2>/dev/null)" = true ]; then
        git fetch --quiet --unshallow origin 2>/dev/null \
            && echo "  fetched the full history (the harness's clone was shallow)"
    fi
else
    # Give this worktree a cargo target of its own: a link to the clone's shared
    # one (what older sessions made) is replaced now, and a missing or empty
    # target/ is copied from the warm snapshot in the background, so the session
    # does not wait for it. A target/ that holds a build is left alone. See
    # scripts/seed-target.sh. "$BASH" is the bash running this hook: a bare
    # `bash` can resolve to WSL's on a Windows PATH. A plain cargo does not
    # wait for that copy (it builds cold, and the copy is thrown away), so the
    # session is told to run the script in the foreground first: it waits.
    if [ -f scripts/seed-target.sh ]; then
        seeding=$("${BASH:-bash}" scripts/seed-target.sh --background 2>&1)
        if [ -n "$seeding" ]; then
            printf '%s\n' "$seeding"
            case "$seeding" in
            *"in the background"*)
                echo "  before your first plain cargo here, run scripts/seed-target.sh: it waits for that copy"
                ;;
            esac
        fi
    fi
    # Keep the primary checkout's main current (silent unless it advanced).
    [ -x scripts/park-primary.sh ] && scripts/park-primary.sh --sync-only 2>/dev/null
fi

# A cloud container starts cold: no installed susi and no target/. The check
# needs the binary, so start the build now, in the background, while the agent
# reads the rules — it is what the agent's first command would do anyway, and
# cargo's own build-directory lock makes a second build wait for it (the clone
# has a target of its own; nothing else builds into it). SUSI_CLOUD_PREBUILD=0
# opts out. The log is in the clone's git directory.
prebuild() {
    local common lock log
    common=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || return 0
    lock="$common/susi-cloud-build.lock"
    log="$common/susi-cloud-build.log"
    (
        exec 9>"$lock"
        flock -n 9 || exit 0
        exec cargo build -q
    ) >"$log" 2>&1 </dev/null &
    disown 2>/dev/null || true
    echo "building susi in the background (log: $log); wait for it with: cargo build -q"
}

if command -v susi >/dev/null 2>&1 && susi workflow --help >/dev/null 2>&1; then
    susi workflow check 2>&1 || true
elif [ -x ./target/debug/susi ]; then
    ./target/debug/susi workflow check 2>&1 || true
elif [ "$cloud" = 1 ] && [ "${SUSI_CLOUD_PREBUILD:-1}" != 0 ] && command -v cargo >/dev/null 2>&1 \
    && command -v flock >/dev/null 2>&1; then
    prebuild
else
    echo "susi workflow check is not installed here; run: scripts/seed-target.sh; cargo run -q -- workflow check"
fi
if [ "$cloud" = 1 ]; then
    cat <<MSG

Cloud session: this clone is your worktree (branch $(git symbolic-ref -q --short HEAD 2>/dev/null || echo '<detached>')) — do NOT run scripts/susi-worktree.sh,
and never ask for the operator's laptop (Remote Control, a computer folder, a button): everything runs here.
Then: target/debug/susi workflow check && target/debug/susi tasks list && target/debug/susi tasks claim <id> --scope <path>,
end every commit with \`Task: <id>\`, and publish with target/debug/susi workflow finish <id>. Rules: AGENTS.md; docs/cloud-sessions.md.
MSG
else
    cat <<'MSG'

Not ready? In the primary checkout or on main: scripts/susi-worktree.sh   (no name needed; it prints `cd <path>`)
Then: susi tasks list && susi tasks claim <id> --scope <path>, and end every commit with `Task: <id>`. Rules: AGENTS.md (START HERE).
MSG
fi
exit 0
