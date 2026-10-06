#!/usr/bin/env bash
# Install the mold linker on a GitHub-hosted Linux runner.
#
# `.cargo/config.toml` points every Linux target at `.cargo/fast-linker`, which
# uses `ld.mold` when it is on PATH and falls back to the system linker when it
# is not. The root-cli shard alone links 71 integration-test binaries, each
# carrying the whole dependency tree, so a job without mold pays for that with the
# slower default linker - and until this was a script only the test shards had it,
# while the branch compile check, the e2e job, the doc-test/clippy job and the
# post-merge acceptance job all linked without it.
#
#   scripts/ci-install-mold.sh
set -euo pipefail

VER=2.40.4
ARCH="$(uname -m)"
case "$ARCH" in
    x86_64 | aarch64) ;;
    *)
        echo "Skipping mold on $ARCH"
        exit 0
        ;;
esac

if command -v ld.mold >/dev/null 2>&1 && ld.mold --version 2>/dev/null | grep -q "mold ${VER}"; then
    echo "mold ${VER} already installed"
    exit 0
fi

NAME="mold-${VER}-${ARCH}-linux"
curl -sSfL "https://github.com/rui314/mold/releases/download/v${VER}/${NAME}.tar.gz" |
    sudo tar -xz -C /usr/local --strip-components=1
mold --version
command -v ld.mold
