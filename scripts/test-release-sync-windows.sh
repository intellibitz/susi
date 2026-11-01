#!/usr/bin/env bash
set -euo pipefail
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$(cygpath -w "$(dirname "$0")/test-release-sync-windows.ps1")" ;;
    *) echo 'Windows release promotion mechanics require Windows; shell syntax checked here.'; bash -n "$(dirname "$0")/susi-release-sync.sh" ;;
esac
