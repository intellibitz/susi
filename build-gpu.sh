#!/usr/bin/env sh
# SUSI GPU-aware dev build: `cargo xb --release` with the host's GPU backend
# (cuda / metal) detected by xtask. Builds into target/ only — it never
# installs. The local susi is updated from releases by
# scripts/susi-release-sync.sh.

set -eu
exec cargo xb --release "$@"
