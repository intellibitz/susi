#!/usr/bin/env bash
# Installer enables auto-update + daemon without extra flags (contract).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HELPER="$ROOT/crates/susi-config/src/zc_installer.rs"
[[ -f "$HELPER" ]] || { echo "missing zc_installer.rs" >&2; exit 1; }
grep -q 'auto_update' "$HELPER"
grep -q 'daemon' "$HELPER"
grep -q 'default_install_flags\|InstallFlags' "$HELPER"
echo "check-zc-installer: ok"
