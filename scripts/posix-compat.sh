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
# 1a. susi_cd() — under MSYS2 `cd` returns "Device or resource busy" while a
# native Windows process transiently holds the target directory (a CWD or
# handle without delete-sharing breaks Cygwin's chdir). The window is
# transient, so retry for a bounded period before surfacing the real error.
susi_cd() {
    local target=$1 tries=0
    until cd "$target" 2>/dev/null; do
        tries=$((tries + 1))
        if [ "$tries" -ge 300 ]; then
            cd "$target"   # final attempt surfaces the real error
            return
        fi
        sleep 0.2
    done
}
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
        lock="$target.flock"
        if [ "$unlock" = 1 ]; then
            # rmdir of the long-gone .flock.d scheme must not leak its
            # nonzero status — callers run under `set -e`.
            rm -f "$lock" "$target.flock.d/pid" 2>/dev/null || true
            rmdir "$target.flock.d" 2>/dev/null || true
            return 0
        fi
        # The lock is a noclobber-created FILE carrying the owner pid:
        # `set -C` makes `>` fail on an existing name, so create and pid
        # publish are one atomic step — no mkdir→write window a reaper
        # could steal mid-publish. Reclaim is plain `rm`: losing reapers
        # just remove nothing, and the single create that follows decides
        # ownership, so two contenders can never both hold it.
        while true; do
            if (set -C; echo $$ >"$lock") 2>/dev/null; then
                return 0
            fi
            owner=$(cat "$lock" 2>/dev/null) || owner=""
            if [ -n "$owner" ]; then
                # A dead holder's file is reclaimed the same way a real
                # flock's fd release would free the lock on process exit.
                if ! kill -0 "$owner" 2>/dev/null; then
                    rm -f "$lock" 2>/dev/null
                    continue
                fi
            else
                # An empty file is a holder killed mid-create. Give the
                # create's echo a grace window, then reclaim.
                sleep 1
                owner=$(cat "$lock" 2>/dev/null) || owner=""
                if [ -z "$owner" ]; then
                    rm -f "$lock" 2>/dev/null
                    continue
                fi
                if ! kill -0 "$owner" 2>/dev/null; then
                    rm -f "$lock" 2>/dev/null
                fi
                continue
            fi
            [ "$nonblock" = 1 ] && return 1
            sleep 0.2
        done
    }
fi
