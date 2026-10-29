#!/usr/bin/env bash
# Retired. Kept as a pass-through for one release.
#
#   scripts/target-guard.sh --run CMD [ARG...]  runs CMD; exits with its status
#   scripts/target-guard.sh --hold PID          holds nothing
#   scripts/target-guard.sh --release PID       does nothing
#   scripts/target-guard.sh                     holds nothing
#
# Worktrees shared one cargo target, and this guard tried to keep a crate
# another worktree had built from being linked into this one's build: stamps of
# each member's content, a lock held through every gate. It could not close the
# gap (a cargo outside the guard, the harness, an IDE's check, test binaries
# that bake the building worktree's CARGO_MANIFEST_DIR, two Git installs' pid
# tables), and every build on the host queued behind its lock. Each worktree
# now has a target of its own, seeded from a warm snapshot
# (scripts/seed-target.sh), where plain cargo is correct, so there is nothing
# left to guard.
#
# Callers that still use it, until they are gone: the installed susi
# (v0.21.23), whose `tasks close` runs a cargo acceptance through `--run`, and
# branches cut before the guard was retired, whose gate and pre-push hook
# `--hold` and `--release`. Every one of them is about to run cargo, so a
# checkout whose target/ is still a link to the old shared target gets a
# target of its own first (scripts/seed-target.sh, in the foreground: the link
# only is removed, and the copy is waited for), and the cargo that follows
# builds there instead of where a sibling's build can stand in for this one's.
# So does a missing or empty target/: a hook's background copy may be filling
# it (v0.21.23's `tasks close` right after `workflow start`), and a cargo that
# does not wait for that copy builds cold while the copy is thrown away. A
# target/ that holds a build is left as it is, and nothing is run for it. With
# CARGO_TARGET_DIR naming another directory the command builds there, and the
# script copies nothing (it still replaces a link).
# POSIX sh: it may be started by a `sh` that is dash. On Windows it may be Git's
# raw usr/bin shell with System32 ahead of Git on PATH, where a bare `bash` is
# WSL's: /usr/bin goes first before anything is looked up. The scripts'
# directory is taken from $0, and on Windows $0 is the path a Rust caller
# joined: v0.21.23's `tasks close` runs `<worktree>\scripts\target-guard.sh`
# (`F:/w\scripts\target-guard.sh` from a `git rev-parse` path, all backslashes
# from a native one), so its backslashes are made slashes first. The command
# --run passes through gets the caller's PATH back, unchanged.
caller_path=$PATH
self=$0
case "${OSTYPE:-}" in
msys* | cygwin*)
    PATH=/usr/bin:$PATH
    export PATH
    self=$(printf '%s\n' "$0" | tr '\\' /) && [ -n "$self" ] || self=$0
    ;;
esac
mode=
case "${1:-}" in
--run)
    shift
    [ "${1:-}" != -- ] || shift
    if [ $# -eq 0 ]; then
        echo 'usage: target-guard.sh --run CMD [ARG...]' >&2
        exit 2
    fi
    mode=run
    ;;
--hold) mode=hold ;;
--release) exit 0 ;;
'') ;;
*)
    echo 'usage: target-guard.sh [--run CMD [ARG...] | --hold PID | --release PID]' >&2
    exit 2
    ;;
esac
root=$(git rev-parse --show-toplevel 2>/dev/null) || root=
case "$self" in
*/*) seeder=${self%/*}/seed-target.sh ;;
*) seeder=./seed-target.sh ;;
esac
# Whether the target/ in $1 is one to seed: a link, missing, or holding
# nothing a build wrote (an empty one, or cargo's tag files only).
wants_seed() {
    [ -L "$1/target" ] && return 0
    [ -e "$1/target" ] || return 0
    [ -d "$1/target" ] || return 1
    for entry in "$1/target"/* "$1/target"/.[!.]* "$1/target"/..?*; do
        [ -e "$entry" ] || [ -L "$entry" ] || continue
        case ${entry##*/} in
        CACHEDIR.TAG | .rustc_info.json) continue ;;
        esac
        return 1
    done
    return 0
}
shell=${BASH:-bash}
if [ -n "$root" ] && wants_seed "$root" && [ -f "$seeder" ] && command -v "$shell" >/dev/null 2>&1; then
    # Its report goes to stderr: `--hold`'s stdout is read by its caller.
    "$shell" "$seeder" "$root" >&2 </dev/null || true
fi
[ "$mode" = run ] || exit 0
PATH=$caller_path
export PATH
exec "$@"
