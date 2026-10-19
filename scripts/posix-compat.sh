# POSIX-tool compatibility for hosts without a full unix userland (Git
# Bash/MSYS2 on Windows). Source after `set -euo pipefail`:
#   [ ! -f "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh" ] || . "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh"
#
# Two shims:
#
# 1. jq() — the native Windows jq build (winget jqlang.jq) writes CRLF line
#    endings; every `jq -r ... | while read` loop and `$(jq -r ...)` capture in
#    these scripts then compares values carrying a trailing \r and silently
#    mismatches. This wrapper normalizes output to LF.
if command -v jq >/dev/null 2>&1; then
    jq() {
        command jq "$@" | tr -d '\r'
    }
fi
#
# 1b. link_dir() — MSYS2's `ln -s` silently deep-copies unless winsymlinks is
# set; a native symlink needs Developer Mode while a directory junction does
# not. The junction is therefore the fallback when the real link fails. Only
# for directories (cargo `target/` sharing).
link_dir() {
    local shared=$1 link=$2
    if [ -n "${MSYSTEM:-}" ]; then
        MSYS=winsymlinks:nativestrict ln -sfn "$shared" "$link" 2>/dev/null && return 0
        [ -L "$link" ] && return 0
        local winlink winshared
        winlink=$(cygpath -w "$link" 2>/dev/null || printf '%s' "$link")
        winshared=$(cygpath -w "$shared" 2>/dev/null || printf '%s' "$shared")
        cmd //c "mklink /J \"$winlink\" \"$winshared\"" >/dev/null 2>&1
    else
        ln -sfn "$shared" "$link" 2>/dev/null
    fi
}
#
# 2. flock — util-linux flock(1) does not exist on Git Bash/MSYS2, where these
# workflow scripts must run for Windows hosts. When flock is absent this
# defines a
# mkdir-based fallback with the same call shape used here:
#   exec N>"$lock"; flock N     # blocking lock
#   flock -n N                  # non-blocking lock
#   flock -u N                  # unlock
# mkdir is atomic on every filesystem we run on, and /proc/self/fd resolves
# the fd to its lockfile path under MSYS2. The holder's pid is recorded so a
# crashed locker's directory is reclaimed instead of deadlocking the loop.
if ! command -v flock >/dev/null 2>&1; then
    flock() {
        local nonblock=0 unlock=0 fd target dir owner
        while [ $# -gt 1 ]; do
            case "$1" in
                -n) nonblock=1 ;;
                -u) unlock=1 ;;
            esac
            shift
        done
        fd=$1
        target=$(readlink "/proc/self/fd/$fd" 2>/dev/null) || return 1
        dir="$target.flock.d"
        if [ "$unlock" = 1 ]; then
            rm -f "$dir/pid"
            rmdir "$dir" 2>/dev/null
            return 0
        fi
        while true; do
            if mkdir "$dir" 2>/dev/null; then
                echo $$ >"$dir/pid"
                return 0
            fi
            owner=$(cat "$dir/pid" 2>/dev/null) || owner=""
            if [ -n "$owner" ]; then
                # A dead holder's dir is reclaimed the same way a real
                # flock's fd release would free the lock on process exit.
                if ! kill -0 "$owner" 2>/dev/null; then
                    rm -f "$dir/pid"
                    rmdir "$dir" 2>/dev/null
                    continue
                fi
            else
                # The holder was killed between mkdir and the pid write —
                # without a grace window this stale dir would wedge every
                # later acquisition (blocking spins, -n always fails).
                # Wait out the write race once, then reclaim.
                sleep 1
                owner=$(cat "$dir/pid" 2>/dev/null) || owner=""
                if [ -z "$owner" ]; then
                    rmdir "$dir" 2>/dev/null
                    continue
                fi
                if ! kill -0 "$owner" 2>/dev/null; then
                    rm -f "$dir/pid"
                    rmdir "$dir" 2>/dev/null
                fi
                continue
            fi
            [ "$nonblock" = 1 ] && return 1
            sleep 0.2
        done
    }
fi
