#!/usr/bin/env sh
set -eu
exec powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/test-local-channel.ps1 "$@"
