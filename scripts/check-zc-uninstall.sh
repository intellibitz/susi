#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HELPER="$ROOT/crates/susi-config/src/zc_uninstall.rs"
[[ -f "$HELPER" ]] || { echo "missing zc_uninstall.rs" >&2; exit 1; }
grep -q 'uninstall_paths\|UninstallPlan' "$HELPER"
grep -q 'bin/susi\|config\|.susi' "$HELPER"
echo "check-zc-uninstall: ok"
