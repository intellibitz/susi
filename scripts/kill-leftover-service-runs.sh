#!/usr/bin/env bash
# Kill test-spawned susi processes left behind by the gate.
#
# The suite's first `susi <command>` auto-starts the host daemon; that daemon
# spawns `<susi> service-run <name>` leaf children. The supervisor now places
# them in a kill-on-close Windows job object so they cannot outlive it, but a
# leftover from an older build — or a daemon that died without running
# `shutdown_all` (crash, `taskkill /F`, its throwaway HOME removed under it) —
# keeps `target\debug\susi.exe` locked and the harness's stdout pipe open, and
# the next `cargo build`/`cargo test` fails with "Access is denied".
#
# Scope: only `service-run` / `daemon-start` processes launched from THIS
# checkout's `target/`. A sibling agent's daemon (a different worktree path), a
# user's installed `~/.susi/bin/susi`, and the running gate itself are never
# touched. On Linux, where a worktree's target/ may still be a symlink to one
# shared target (worktrees made before each got its own; see
# scripts/seed-target.sh), the process must additionally carry a throwaway
# `SUSI_HOME` (a temp dir), which is what the hermetic wrapper exports.
#
# Best-effort by design: a sweep that cannot identify a process leaves it
# alone. Always exits 0 so teardown never masks the real result.
set -uo pipefail

root=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
target="$root/target"
[ -d "$target" ] || exit 0

case "${OSTYPE:-}" in
msys* | cygwin*)
    if command -v tasklist.exe >/dev/null 2>&1; then
        MSYS2_ARG_CONV_EXCL='*' tasklist.exe /FI "IMAGENAME eq susi.exe" /NH 2>/dev/null | grep -qi "susi.exe" || exit 0
    fi
    command -v powershell.exe >/dev/null 2>&1 || exit 0
    target_win=$(cygpath -aw "$target" 2>/dev/null) || target_win="$target"
    SUSI_SWEEP_TARGET="$target_win" powershell.exe -NoProfile -NonInteractive -Command '
        $t = [IO.Path]::GetFullPath($env:SUSI_SWEEP_TARGET).TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
        Get-CimInstance Win32_Process -Filter "Name = '\''susi.exe'\''" |
            Where-Object {
                $_.Name -eq "susi.exe" -and $_.ExecutablePath -and
                ($_.ExecutablePath -like ($t + "*")) -and
                ($_.CommandLine -match "(^|\s)(service-run|daemon-start)(\s|$)")
            } |
            ForEach-Object {
                Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
                Write-Output $_.ProcessId
            }
    ' 2>/dev/null | tr -d '\r' | while IFS= read -r pid; do
        [ -n "$pid" ] && echo "sweep: killed leftover susi process $pid" >&2
    done
    ;;
*)
    [ -d /proc ] || exit 0
    real_target=$(readlink -f "$target" 2>/dev/null) || real_target="$target"
    for entry in /proc/[0-9]*; do
        pid=${entry#/proc/}
        [ "$pid" = "$$" ] && continue
        cmdline=$(tr '\0' ' ' <"$entry/cmdline" 2>/dev/null) || continue
        case " $cmdline " in
        *" service-run "* | *" daemon-start "*) ;;
        *) continue ;;
        esac
        exe=$(readlink -f "$entry/exe" 2>/dev/null) || continue
        case "$exe" in
        "$real_target"/*) ;;
        *) continue ;;
        esac
        home=$(tr '\0' '\n' <"$entry/environ" 2>/dev/null | sed -n 's/^SUSI_HOME=//p' | head -n 1)
        case "$home" in
        /tmp/* | /var/tmp/* | "${TMPDIR:-/tmp}"/*) ;;
        *) continue ;;
        esac
        if kill -TERM "$pid" 2>/dev/null; then
            echo "sweep: killed leftover susi process $pid" >&2
        fi
    done
    ;;
esac

exit 0
