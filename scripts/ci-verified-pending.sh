#!/usr/bin/env bash
# ci-verified-pending.sh — exit 0 when merged tasks still lack a verified
# record, meaning `susi tasks verify-merged` has work to do; exit 1 when the
# attestation queue is drained and the job can skip its toolchain build.
#
# verify's own audit computes the same set after building susi (~5-15 min
# warm). The refs answer it in seconds, so asking first is what a no-op
# should cost. The failure direction is always "pending": any remote read
# error exits 0 and the job builds and decides for real rather than let an
# attestation starve on a flaky network.
set -uo pipefail

merged=$(git ls-remote origin 'refs/merged/*' 2>/dev/null | sed 's|.*/||' | sort -u) || exit 0
verified=$(git ls-remote origin 'refs/verified/*' 2>/dev/null | sed 's|.*/||' | sort -u) || exit 0
[ -n "$(comm -23 <(printf '%s\n' "$merged") <(printf '%s\n' "$verified"))" ]
