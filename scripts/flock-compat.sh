# flock compatibility shim — source this after `set -euo pipefail`:
#   . "$(dirname "${BASH_SOURCE[0]}")/flock-compat.sh"
#
# util-linux flock(1) does not exist on Git Bash/MSYS2, where these workflow
# scripts must run for Windows hosts. When flock is absent this defines a
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
            if [ -n "$owner" ] && ! kill -0 "$owner" 2>/dev/null; then
                rm -f "$dir/pid"
                rmdir "$dir" 2>/dev/null
                continue
            fi
            [ "$nonblock" = 1 ] && return 1
            sleep 0.2
        done
    }
fi
