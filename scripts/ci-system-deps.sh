#!/usr/bin/env bash
# Native build dependencies the workspace needs on a GitHub-hosted Ubuntu runner:
# pkg-config, the OpenSSL headers and cmake.
#
# The runner image already ships all three, yet every job used to run
# `apt-get update && apt-get install` unconditionally - 15-25 s of index refresh
# per job, on seven jobs of every run, for packages that were already there. This
# installs only what is actually missing, so the common case is a few dpkg
# lookups and a job that does need a package still gets it.
#
#   scripts/ci-system-deps.sh
set -euo pipefail

command -v dpkg >/dev/null 2>&1 || {
    echo "ci-system-deps.sh targets the Ubuntu runner image (no dpkg here)" >&2
    exit 1
}

want=(pkg-config libssl-dev cmake)
missing=()
for pkg in "${want[@]}"; do
    dpkg -s "$pkg" >/dev/null 2>&1 || missing+=("$pkg")
done

if [ "${#missing[@]}" -eq 0 ]; then
    echo "system dependencies already present: ${want[*]}"
    exit 0
fi

echo "installing missing system dependencies: ${missing[*]}"
sudo apt-get update
sudo apt-get install -y "${missing[@]}"
