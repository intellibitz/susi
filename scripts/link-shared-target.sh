#!/usr/bin/env bash
# Retired: worktrees no longer link target/ to the primary checkout's shared
# one. Each has its own, copied from a warm snapshot (scripts/seed-target.sh).
#
#   scripts/link-shared-target.sh [dir]     same as scripts/seed-target.sh [dir]
#
# Kept for one release, because the installed susi (v0.21.23) still runs it
# after `workflow start` (its harness) and its `workflow start` still links the
# shared target: seeding replaces that link with the worktree's own target.
# Delete it once no release calls it. It never fails a caller.
#
# That harness runs it as `sh <repo>\scripts\link-shared-target.sh` (a path
# Rust joined: `F:/r\scripts\...` from a `git rev-parse` path, all backslashes
# from a native one), which on Windows can be Git's raw usr/bin shell with
# System32 ahead of Git on PATH (a bare `bash` there is WSL's): /usr/bin goes
# first before anything is looked up, and the backslashes in $0 are made
# slashes before the script's directory is taken from it.
self=$0
case "${OSTYPE:-}" in
msys* | cygwin*)
    PATH=/usr/bin:$PATH
    export PATH
    self=$(printf '%s\n' "$0" | tr '\\' /) && [ -n "$self" ] || self=$0
    ;;
esac
case "$self" in
*/*) here=${self%/*} ;;
*) here=. ;;
esac
[ -f "$here/seed-target.sh" ] || exit 0
if [ -n "${BASH_VERSION:-}" ]; then
    exec "$BASH" "$here/seed-target.sh" "$@"
fi
command -v bash >/dev/null 2>&1 || exit 0
exec bash "$here/seed-target.sh" "$@"
