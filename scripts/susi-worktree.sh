#!/usr/bin/env bash
# Mandate 49: create your own worktree + branch off the latest origin/main,
# ready for the atomic per-task loop (sync→claim→work→commit→sync→push→sync).
#
#   scripts/susi-worktree.sh              auto name: <tool>-<timestamp>-<pid>
#   scripts/susi-worktree.sh <name> [base]   (base defaults to origin/main)
#
# It fetches, creates the worktree, installs the hooks and merge driver in it,
# parks the primary checkout at origin/main, and prints `cd <path>` as the LAST
# line so a caller can `eval "$(scripts/susi-worktree.sh | tail -1)"`.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
common=$(git rev-parse --path-format=absolute --git-common-dir)
# Startup changes shared refs/config; serialize it within this clone.
exec 9>"$common/susi-worktree-start.lock"
flock 9
primary=$(dirname "$common")

# The agent tool this process runs inside, named by its ancestors' executables
# (the same markers as zc_agent_identity::detect_agent). It is never the git
# login: that is one name shared by every agent on the machine, and a worker
# named after it came out INTELLIBITZ<timestamp> — the human, not the agent.
detect_tool() {
    local pid=$$ args exe
    local i
    for i in 1 2 3 4 5 6; do
        pid=$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d '[:space:]') || return 0
        { [ -n "$pid" ] && [ "$pid" -gt 1 ]; } 2>/dev/null || return 0
        args=$(ps -o args= -p "$pid" 2>/dev/null) || return 0
        # The program's basename and its first argument (`node …/claude`), as
        # the Rust side reads them: a directory in the path names nothing.
        exe=$(printf '%s\n' "$args" | awk '{ n = split($1, p, "/"); print tolower(p[n]) " " tolower($2) }')
        case $exe in
        *cursor*) echo cursor; return 0 ;;
        *claude* | *anthropic*) echo claude; return 0 ;;
        *codex*) echo codex; return 0 ;;
        *antigravity*) echo antigravity; return 0 ;;
        *gemini*) echo gemini; return 0 ;;
        *devin*) echo devin; return 0 ;;
        *aider*) echo aider; return 0 ;;
        esac
    done
}

agent=${SUSI_AGENT:-$(detect_tool)}
agent=$(printf '%s' "$agent" | tr '[:upper:]' '[:lower:]' | tr -c 'a-z0-9\n' '-' | sed 's/^-*//; s/-*$//')
name=${1:-"${agent:-agent}-$(date +%Y%m%d-%H%M%S)-${BASHPID:-$$}"}
base=${2:-origin/main}
case "$name" in
main | HEAD | "") echo "susi-worktree: refusing branch name '$name'" >&2; exit 2 ;;
esac
# The identity this worktree will carry; also what the reuse check compares.
worker=$(printf '%s' "$name" | tr '[:lower:]' '[:upper:]' | tr -cd 'A-Z0-9')

git -C "$root" fetch -q origin
dest="$(dirname "$primary")/$(basename "$primary")-$name"
if [ -d "$dest" ]; then
    [ "$(git -C "$dest" rev-parse --path-format=absolute --git-common-dir)" = "$common" ] \
        && [ "$(git -C "$dest" symbolic-ref -q --short HEAD)" = "$name" ] \
        || { echo "susi-worktree: refusing to reuse an unrelated checkout" >&2; exit 1; }
    # Two workers in one directory is two workers sharing one token and one
    # branch: they can renew, close and release each other's claim, so this is
    # the one collision that must never be silent.
    owner=$(git -C "$dest" config --worktree --get susi.agent 2>/dev/null || true)
    if [ -n "$owner" ] && [ "$owner" != "$worker" ]; then
        cat >&2 <<MSG
❌ $dest already belongs to agent $owner, not $worker.
   One worktree per worker: pick a different name, or reuse it as $owner.
MSG
        exit 1
    fi
    echo "susi-worktree: $dest already exists — reusing it" >&2
else
    git -C "$root" worktree add -b "$name" "$dest" "$base" >&2
fi
echo "worktree: $dest (branch $name)" >&2

# One cargo target dir per clone, not one per worktree: a fresh worktree used
# to cold-compile the whole dependency tree (~25 min) and keep its own ~20 GB
# copy — one clone's worktrees alone held ~95 GB of mostly-identical
# artifacts. The primary checkout is parked and still, so its target/ is the
# shared cache. A symlink keeps `target/...` relative paths working; an
# existing real target/ is left alone, and SUSI_PRIVATE_TARGET=1 opts out.
# `**/target` in .gitignore ignores the link itself.
#
# Sharing only pays while no tracked cargo config embeds a worktree path. A
# repo-relative `linker` in .cargo/config.toml did: cargo made it an absolute path
# inside each worktree and hashed it into every unit, so two worktrees shared
# nothing and overwrote each other's artifacts (a fresh one compiled 716 crates in
# 656 s). Without it, one created after a warm build compiled 81 in 176 s;
# tests/ci_workflow_perf.rs pins the config. A worktree made another way (the
# desktop app) keeps a private target/ and pays the cold build once.
shared_target=$primary/target
if [ -z "${SUSI_PRIVATE_TARGET:-}" ] && { [ ! -e "$dest/target" ] || [ -L "$dest/target" ]; }; then
    mkdir -p "$shared_target"
    ln -sfn "$shared_target" "$dest/target"
    echo "target: shared -> $shared_target" >&2
fi

# Persist a unique worker identity, even when all tools share one Git login.
#
# `git worktree add` copies the *creating* worktree's worktree-config, so a new
# worktree silently inherits that agent's identity and Git author: three
# worktrees provisioned from a DEEPSEEK tree all came out as DEEPSEEK, and the
# guard below then declined to overwrite a value that was never theirs. Clear
# what was inherited first, then state this worker's own — one token per worker
# is what keeps two agents from renewing, closing or releasing each other's
# claim (Mandate 50).
git -C "$root" config extensions.worktreeConfig true
for key in susi.agent user.name user.email; do
    git -C "$dest" config --worktree --unset-all "$key" 2>/dev/null || true
done
git -C "$dest" config --worktree susi.agent "$worker"
git -C "$dest" config --worktree user.name "$worker"
git -C "$dest" config --worktree user.email "$(printf '%s' "$worker" | tr '[:upper:]' '[:lower:]')@localhost"

# Hooks + ledger merge driver for the clone (shared by every worktree of it).
(cd "$dest" && scripts/setup-dev.sh) >&2

# Keep the primary checkout parked at origin/main so it never drifts onto a
# stale branch that invites work (non-fatal: it refuses if it has real work).
"$root/scripts/park-primary.sh" >&2 || echo "note: primary checkout not parked (see above); it is still not for work" >&2

# A remote merge cannot invoke local hooks: keep a local singleton watcher.
if [ -x "$dest/scripts/parallel-workflow.sh" ] && [ -z "${SUSI_PRIMARY_WATCH_DISABLE:-}" ]; then
    (cd "$dest" && scripts/parallel-workflow.sh start-watch) 9>&-
fi

echo "ready: atomic loop — \`susi workflow check\`, then sync→claim one→work→commit→sync→push→sync" >&2
echo "cd $dest"
