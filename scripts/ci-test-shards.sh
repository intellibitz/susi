#!/usr/bin/env bash
# The shard -> package-list map the test.yml `test` matrix runs, one
# `name<TAB>cargo package args<TAB>partitions` line per shard. It is the
# single source: the `codetree` job builds the job's matrix from it (`--matrix`)
# with the shards a merge cannot affect left out — a shard's tests can only
# break when the merge touched one of the shard's crates or something one of
# them depends on, exactly the changed-crates-plus-dependents set — so a leg
# that would only re-prove green code is never allocated a runner. A shard
# whose third column is more than 1 runs on that many runners, a hash
# partition each (`--partition hash:k/n`): hash partitioning spreads root-cli's
# ~100 small test binaries evenly, where counted partitioning goes binary by
# binary and piles the first test of each onto partition 1. The legs keep the
# shard's name, so scoping and the cache key (`test-<name>`) are per shard.
# xtask/tests/ci_workflow_perf.rs pins this file against test.yml.
#
#   scripts/ci-test-shards.sh                  name<TAB>pkgs<TAB>partitions per line
#   scripts/ci-test-shards.sh --json           [{"name", "pkgs", "partitions"}, ...]
#   scripts/ci-test-shards.sh --matrix [SKIP]  {"shard": [{"name", "pkgs"[, "partition"]}, ...]}
#                                              — the test.yml matrix, without the
#                                              shards named in SKIP (a JSON array;
#                                              `-` reads it from stdin)
#
# `-` is for a caller that is not a shell. Git for Windows' sh.exe, started
# from a native process (cargo test, PowerShell), globs its own unquoted
# command-line words before the script runs, and the empty array `[]` - the
# common call - is an unterminated bracket expression that crashed it
# (0xC0000005, exit status non-zero, nothing on stderr). Text on stdin is never
# globbed. test.yml passes the array as an argument, from bash, where none of
# this happens.
#
# Every workspace member is in some shard: a member in none has its tests run
# by no CI job at all (xtask checks the members against
# this map). Keep shards roughly balanced: gemi pulls the candle/ort vendors,
# and holds susi-harness, whose closure (susi-adapters-llm, the HTTP transport)
# it already builds; daemon the service shells, gawd the swarm crates, and
# susi-leaf-services --all-features (wasmtime's cranelift) is its own leg
# because as a second step of `foundation` it made that shard ~10 min against
# ~2.5 min for the rest of it. root-cli was the longest shard of main's
# post-merge suite, 17-45 min, of which compiling is about 2.5 and the rest is
# nextest (534 tests, 76 of them gmcp_requirements at a median 88 s): it runs
# in three partitions.
set -euo pipefail
[ ! -f "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh" ] || . "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh"

shards=$(cat <<'SHARDS'
foundation	-p susi-paths -p susi-error -p susi-config -p susi-abi -p susi-core -p susi-sandbox -p susi-sandbox-client -p susi-http-transport -p susi-native-client -p susi-leaf-services -p susi-unsafe-ratchet -p susi-windows-runtime
daemon	-p susi-daemon -p susi-server -p susi-gmcp -p susi-tools
gawd	-p susi-gawd -p susi-gawd-agents -p susi-gawd-swarm -p susi-gawd-a2a -p susi-agents
gemi	-p susi-gemi -p susi-gemi-models -p susi-adapters-llm -p susi-harness
vendor-cells	-p susi-vendor-wasmtime -p susi-vendor-candle -p susi-vendor-fastembed -p susi-vendor-tantivy -p susi-vendor-chrome -p susi-vendor-syn -p susi-vendor-mcp -p susi-vendor-mcp-server -p susi-vendor-cloud -p susi-vendor-agents -p susi-vendor-models -p susi-vendor-web -p susi-dsh-cell -p susi-universal-cell -p susi-vendor-anyhow -p susi-vendor-axum -p susi-vendor-base64 -p susi-vendor-bollard -p susi-vendor-bytes -p susi-vendor-chacha20poly1305 -p susi-vendor-clap -p susi-vendor-console-subscriber -p susi-vendor-criterion -p susi-vendor-dashmap -p susi-vendor-ed25519-dalek -p susi-vendor-flume -p susi-vendor-futures -p susi-vendor-getrandom -p susi-vendor-hex -p susi-vendor-http-body-util -p susi-vendor-hyper -p susi-vendor-hyper-util -p susi-vendor-indicatif -p susi-vendor-jsonschema -p susi-vendor-libc -p susi-vendor-md-5 -p susi-vendor-parking-lot -p susi-vendor-proptest -p susi-vendor-ra2a -p susi-vendor-rayon -p susi-vendor-rcgen -p susi-vendor-regex -p susi-vendor-serde -p susi-vendor-serde-json -p susi-vendor-sha1 -p susi-vendor-sha2 -p susi-vendor-shlex -p susi-vendor-signal-hook -p susi-vendor-sysinfo -p susi-vendor-tempfile -p susi-vendor-tokio -p susi-vendor-tokio-rustls -p susi-vendor-tokio-stream -p susi-vendor-tower -p susi-vendor-tower-service -p susi-vendor-tracing -p susi-vendor-tracing-appender -p susi-vendor-tracing-subscriber -p susi-vendor-ureq -p susi-vendor-url -p susi-vendor-winapi -p susi-vendor-windows-registry -p susi-vendor-x25519-dalek
root-cli	-p susi -p xtask	3
leaf-all-features	-p susi-leaf-services --all-features
SHARDS
)

case "${1:-}" in
    "") printf '%s\n' "$shards" ;;
    --json)
        printf '%s\n' "$shards" | jq -R 'split("\t") | {name: .[0], pkgs: .[1], partitions: ((.[2] // "1") | tonumber)}' | jq -s .
        ;;
    --matrix)
        skip=${2:-}
        [ "$skip" != - ] || skip=$(cat)
        skip=${skip:-[]}
        printf '%s\n' "$shards" | jq -R -c --argjson skip "$skip" '
            split("\t") | {name: .[0], pkgs: .[1], partitions: ((.[2] // "1") | tonumber)}
            | select(.name as $n | $skip | index($n) | not)
            | .partitions as $n | .name as $name | .pkgs as $pkgs
            | if $n > 1 then range(1; $n + 1) | {name: $name, pkgs: $pkgs, partition: "hash:\(.)/\($n)"}
              else {name: $name, pkgs: $pkgs} end' | jq -sc '{shard: .}'
        ;;
    *)
        echo "usage: ci-test-shards.sh [--json | --matrix [skip-json-array | -]]" >&2
        exit 2
        ;;
esac
