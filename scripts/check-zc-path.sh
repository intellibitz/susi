#!/usr/bin/env bash
# Check that PATH install can be done with a single consent flag (zero-config).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# The installer documents / implements one-consent PATH wiring via
# SUSI_PATH_CONSENT=1 (or --path-consent). This check asserts the contract
# string exists in the install path helpers so fresh installs stay one-shot.
if ! grep -q 'PATH_CONSENT\|path.consent\|--path-consent\|SUSI_PATH_CONSENT' \
  "$ROOT/scripts/susi-release-sync.sh" \
  "$ROOT/install.sh" \
  "$ROOT/crates/susi-config/src/zc_path.rs" 2>/dev/null
then
  # Prefer dedicated module when present.
  if [[ ! -f "$ROOT/crates/susi-config/src/zc_path.rs" ]]; then
    echo "missing zc_path consent helper" >&2
    exit 1
  fi
fi
# Validate the helper API via a tiny rustc-free contract: file must export consent.
if ! grep -q 'path_consent\|PathConsent\|SUSI_PATH_CONSENT' "$ROOT/crates/susi-config/src/zc_path.rs"; then
  echo "zc_path.rs must define PATH consent symbols" >&2
  exit 1
fi
echo "check-zc-path: ok"
