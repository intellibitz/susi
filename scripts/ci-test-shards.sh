#!/usr/bin/env bash
# The shard -> package-list map the test.yml `test` matrix runs, one
# `name<TAB>cargo package args` line per shard. It lives here rather than only
# in the workflow so the `codetree` stamp can compute which shards a merge can
# possibly affect: a shard's tests can only break when the merge touched one
# of the shard's crates or something one of them depends on — exactly the
# changed-crates-plus-dependents set. tests/ci_workflow_perf.rs pins this file
# and the test.yml matrix in sync.
#
#   scripts/ci-test-shards.sh            name<TAB>pkgs per line
#   scripts/ci-test-shards.sh --json     [{"name": ..., "pkgs": ...}, ...]
#
# Keep shards roughly balanced: gemi pulls the candle/ort vendors, daemon the
# service shells, gawd the swarm crates, and susi-leaf-services --all-features
# (wasmer's cranelift) is its own leg because as a second step of `foundation`
# it made that shard ~10 min against ~2.5 min for the rest of it.
set -euo pipefail

shards=$(cat <<'SHARDS'
foundation	-p susi-paths -p susi-error -p susi-config -p susi-abi -p susi-core -p susi-sandbox -p susi-sandbox-client -p susi-http-transport -p susi-native-client -p susi-leaf-services
daemon	-p susi-daemon -p susi-server -p susi-gmcp -p susi-tools
gawd	-p susi-gawd -p susi-gawd-agents -p susi-gawd-swarm -p susi-gawd-a2a -p susi-agents
gemi	-p susi-gemi -p susi-gemi-models -p susi-adapters-llm
vendor-cells	-p susi-vendor-wasmer -p susi-vendor-candle -p susi-vendor-fastembed -p susi-vendor-tantivy -p susi-vendor-chrome -p susi-vendor-syn -p susi-vendor-mcp -p susi-vendor-mcp-server -p susi-vendor-cloud -p susi-vendor-agents -p susi-vendor-models -p susi-vendor-web -p susi-dsh-cell -p susi-universal-cell -p susi-vendor-anyhow -p susi-vendor-axum -p susi-vendor-base64 -p susi-vendor-bollard -p susi-vendor-bytes -p susi-vendor-chacha20poly1305 -p susi-vendor-clap -p susi-vendor-console-subscriber -p susi-vendor-criterion -p susi-vendor-dashmap -p susi-vendor-ed25519-dalek -p susi-vendor-flume -p susi-vendor-futures -p susi-vendor-getrandom -p susi-vendor-hex -p susi-vendor-http-body-util -p susi-vendor-hyper -p susi-vendor-hyper-util -p susi-vendor-indicatif -p susi-vendor-jsonschema -p susi-vendor-libc -p susi-vendor-md-5 -p susi-vendor-parking-lot -p susi-vendor-proptest -p susi-vendor-ra2a -p susi-vendor-rayon -p susi-vendor-rcgen -p susi-vendor-regex -p susi-vendor-serde -p susi-vendor-serde-json -p susi-vendor-sha1 -p susi-vendor-sha2 -p susi-vendor-shlex -p susi-vendor-signal-hook -p susi-vendor-sysinfo -p susi-vendor-tempfile -p susi-vendor-tokio -p susi-vendor-tokio-rustls -p susi-vendor-tokio-stream -p susi-vendor-tower -p susi-vendor-tower-service -p susi-vendor-tracing -p susi-vendor-tracing-appender -p susi-vendor-tracing-subscriber -p susi-vendor-ureq -p susi-vendor-url -p susi-vendor-winapi -p susi-vendor-x25519-dalek
root-cli	-p susi -p xtask
leaf-all-features	-p susi-leaf-services --all-features
SHARDS
)

case "${1:-}" in
    "") printf '%s\n' "$shards" ;;
    --json)
        printf '%s\n' "$shards" | jq -R 'split("\t") | {name: .[0], pkgs: .[1]}' | jq -s .
        ;;
    *)
        echo "usage: ci-test-shards.sh [--json]" >&2
        exit 2
        ;;
esac
