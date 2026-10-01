#!/usr/bin/env bash
# Pre-fetch the ONNX Runtime binaries `ort-sys` downloads at build time.
#
# `susi-vendor-fastembed` enables `ort-download-binaries-rustls-tls`, so the
# build performs one unretried GET against pyke's CDN (cached afterwards under
# ~/.cache/ort.pyke.io). A single CDN hiccup therefore fails whichever job
# cold-builds it — which is exactly what reddened main:
#
#   error: ort-sys@2.0.0-rc.13: ort-sys failed to download prebuilt binaries
#          from `https://cdn.pyke.io/...`
#
# `Swatinem/rust-cache` caches `target/`, not that download directory, and a
# second ~100 MB cache entry per job would compete with rust-cache's own
# entries under GitHub's 10 GB per-repo limit — making cold builds *more*
# likely, not less. So this makes the one fetch we cannot avoid survive a
# hiccup: build the crate (and therefore the ONNX Runtime download) up front,
# with bounded retries and backoff, before the job's real run.
#
# Only the build is retried, never the tests, so a genuine test failure still
# fails the job. On a warm cache this is a cargo no-op.
#
#   scripts/ci-prefetch-onnxruntime.sh
set -uo pipefail

cd "$(git rev-parse --show-toplevel)"

attempts="${ORT_PREFETCH_ATTEMPTS:-4}"
for attempt in $(seq 1 "$attempts"); do
    if cargo build -p susi-vendor-fastembed --locked; then
        echo "✅ ONNX Runtime ready (attempt $attempt/$attempts)"
        exit 0
    fi
    delay=$((attempt * 15))
    echo "⚠️  ONNX Runtime fetch failed (attempt $attempt/$attempts); retrying in ${delay}s" >&2
    sleep "$delay"
done

echo "❌ ONNX Runtime still unavailable after $attempts attempts — pyke CDN unreachable?" >&2
exit 1
