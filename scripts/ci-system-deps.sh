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
# "Already there" has to include a tool that is on PATH but not a dpkg package:
# the image carries its own cmake under /usr/local/bin, so `dpkg -s cmake` fails
# on every runner, which made every job refresh the index and install a second,
# older cmake (3.28) that PATH never even picked (12-19 s a job, measured on the
# release workflow). A package therefore counts as present when dpkg knows it or
# when the binary it provides resolves on PATH.
#
#   scripts/ci-system-deps.sh
set -euo pipefail

command -v dpkg >/dev/null 2>&1 || {
    echo "ci-system-deps.sh targets the Ubuntu runner image (no dpkg here)" >&2
    exit 1
}

# package[:binary that proves it]
want=(pkg-config:pkg-config libssl-dev cmake:cmake)
missing=()
names=()
for entry in "${want[@]}"; do
    pkg="${entry%%:*}"
    bin=""
    [ "$pkg" = "$entry" ] || bin="${entry#*:}"
    names+=("$pkg")
    if dpkg -s "$pkg" >/dev/null 2>&1; then
        continue
    fi
    if [ -n "$bin" ] && command -v "$bin" >/dev/null 2>&1; then
        continue
    fi
    missing+=("$pkg")
done

if [ "${#missing[@]}" -eq 0 ]; then
    echo "system dependencies already present: ${names[*]}"
    exit 0
fi

echo "installing missing system dependencies: ${missing[*]}"
sudo apt-get update
sudo apt-get install -y "${missing[@]}"
