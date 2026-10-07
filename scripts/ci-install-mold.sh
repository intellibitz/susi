#!/usr/bin/env bash
# Install the mold linker on a GitHub-hosted Linux runner.
#
# `.cargo/config.toml` points the aarch64 Linux target at `.cargo/fast-linker`,
# which uses `ld.mold` when it is on PATH and falls back to the system linker when
# it is not; that is the release workflow's linux-aarch64 leg. x86_64 sets no
# linker at all (a repo-relative one made every worktree's artifacts incompatible
# with every other's - see the comment there) and links with rustc's bundled lld,
# so on x86_64 jobs this step is harmless but no longer what makes linking fast.
# It stays one shared script so every Linux job's bootstrap is the same.
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
