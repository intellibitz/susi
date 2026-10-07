#!/usr/bin/env bash
# Lists the workspace packages that transitively depend on a given package,
# the package itself included — i.e. every package whose build can pull it in.
#
#   ci-reverse-deps.sh susi-vendor-fastembed
#
# `ci-changed-crates.sh` answers "what did the diff touch, and what depends on
# it"; this answers the converse for a fixed package, so a job can ask "does
# the affected set include anything that builds the ONNX Runtime?" instead of
# paying for its prefetch on every crate-touching push.
#
# Normal, dev and build dependencies all count, as they do for the gate: a
# dev-dependency edge is a test target that pulls the package in.
set -euo pipefail

pkg=${1:?usage: ci-reverse-deps.sh <workspace-package>}

meta=$(cargo metadata --no-deps --format-version 1 2>/dev/null) || exit 1

# dependent|dependency, restricted to edges between workspace packages. A
# dependency's `name` is the package name, never a `package = ` rename.
edges=$(jq -r '
    [.packages[].name] as $ws
    | .packages[] | .name as $dependent
    | .dependencies[] | select(.name as $d | $ws | index($d))
    | $dependent + "|" + .name' <<<"$meta") || exit 1

declare -A seen=([$pkg]=1)
grew=1
while [ "$grew" = 1 ]; do
    grew=0
    while IFS='|' read -r dependent dependency; do
        [ -n "$dependent" ] || continue
        if [ -n "${seen[$dependency]:-}" ] && [ -z "${seen[$dependent]:-}" ]; then
            seen[$dependent]=1
            grew=1
        fi
    done <<<"$edges"
done

for p in "${!seen[@]}"; do
    echo "$p"
done | sort
