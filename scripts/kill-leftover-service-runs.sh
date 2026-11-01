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
# Scope: only test-owned processes launched from THIS checkout's `target/`.
# A checkout-built executable can also serve the user's real instance, so on
# Windows its path alone is insufficient: require a daemon workspace inside
# this invocation's throwaway homes, and only reap its own service children.
# On Linux, where a worktree's target/ may still be a symlink to one
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
    # Only the hermetic wrapper establishes these homes. Fail closed when the
    # ownership marker is missing or inconsistent; a normal SUSI_HOME can be
    # an active developer instance.
    [ -n "${SUSI_HOME:-}" ] && [ "${SUSI_HOME:-}" = "${SUSI_HERMETIC_FORBIDDEN:-}" ] && [ -n "${USERPROFILE:-}" ] || exit 0
    if command -v tasklist.exe >/dev/null 2>&1; then
        MSYS2_ARG_CONV_EXCL='*' tasklist.exe /FI "IMAGENAME eq susi.exe" /NH 2>/dev/null | grep -qi "susi.exe" || exit 0
    fi
    command -v powershell.exe >/dev/null 2>&1 || exit 0
    target_win=$(cygpath -aw "$target" 2>/dev/null) || target_win="$target"
    home_win=$(cygpath -aw "$SUSI_HOME" 2>/dev/null) || exit 0
    user_win=$(cygpath -aw "$USERPROFILE" 2>/dev/null) || exit 0
    SUSI_SWEEP_TARGET="$target_win" SUSI_SWEEP_HOME="$home_win" SUSI_SWEEP_USER="$user_win" powershell.exe -NoProfile -NonInteractive -Command '
        $t = [IO.Path]::GetFullPath($env:SUSI_SWEEP_TARGET).TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
        $roots = @($env:SUSI_SWEEP_HOME, $env:SUSI_SWEEP_USER) | ForEach-Object { [IO.Path]::GetFullPath($_).TrimEnd("\") }
        $processes = @(Get-CimInstance Win32_Process -Filter "Name = '\''susi.exe'\''" | Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($t, [StringComparison]::OrdinalIgnoreCase) })
        $daemons = @($processes | Where-Object {
            if ($_.CommandLine -notmatch '\''(?:^|\s)daemon-start\s+--workspace\s+(?:"([^"]+)"|(\S+))'\'') { return $false }
            $workspace = if ($Matches[1]) { $Matches[1] } else { $Matches[2] }
            try { $workspace = [IO.Path]::GetFullPath($workspace).TrimEnd("\") } catch { return $false }
            foreach ($r in $roots) {
                if ($workspace.Equals($r, [StringComparison]::OrdinalIgnoreCase) -or $workspace.StartsWith($r + "\", [StringComparison]::OrdinalIgnoreCase)) { return $true }
            }
            return $false
        })
        $ids = @($daemons | ForEach-Object { $_.ProcessId })
        $children = @($processes | Where-Object { $ids -contains $_.ParentProcessId -and $_.CommandLine -match "(^|\s)service-run(\s|$)" })
        @($children) + @($daemons) | ForEach-Object {
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
