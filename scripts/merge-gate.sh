#!/usr/bin/env bash
# Run the branch compile gate against the current checkout — in the
# serialized merge job, the trial merge `auto-merge-pr.sh` built for a
# green-but-behind head and checked out in place, so the job's warm cargo
# cache and fingerprints apply. That is what turns a stale-head retest —
# minutes of queue round-trip — into a bounded compile inside the merge job.
#
#   merge-gate.sh <base-sha>
#
# The gate is the same proof the `check` job gives a branch push: harness
# contract, `cargo fmt --all --check` (a merge resolution can produce
# unformatted code), then clippy -D warnings over `--all-targets` for the
# changed crates and their dependents. It compiles, it does not execute
# tests — the head's green run already covered the branch's own tests, and
# the main suite re-runs on the merged tree.
set -uo pipefail

base=${1:?usage: merge-gate.sh <base-sha>}
here=$(cd "$(dirname "$0")" && pwd)
cd "$(git rev-parse --show-toplevel)"

"$here/check-test-harness-contract.sh" || exit 1
cargo fmt --all --check || exit 1

pr_pkgs=$(BASE_REF="$base" "$here/ci-changed-crates.sh" --dependents || echo ALL)
# On a trial merge commit (`HEAD^1` = main, `HEAD^2` = the already-green PR
# head), any crate that does not depend on what changed on `main` since the PR
# branched (`BASE_REF=HEAD^2`) has the exact inputs the PR's own green `check`
# job already proved; only the intersection of the two dependent closures can
# interact across the merge.
if git rev-parse --verify -q HEAD^2 >/dev/null 2>&1; then
    main_pkgs=$(BASE_REF="HEAD^2" "$here/ci-changed-crates.sh" --dependents || echo ALL)
else
    main_pkgs=ALL
fi
if [ "$pr_pkgs" = ALL ]; then
    pkgs=$main_pkgs
elif [ "$main_pkgs" = ALL ]; then
    pkgs=$pr_pkgs
elif [ -z "$pr_pkgs" ] || [ -z "$main_pkgs" ]; then
    pkgs=""
else
    pkgs=$(comm -12 <(sort -u <<<"$pr_pkgs") <(sort -u <<<"$main_pkgs"))
fi
case "$pkgs" in
    ALL)
        "$here/ci-prefetch-onnxruntime.sh" || exit 1
        cargo clippy --workspace --all-targets --locked -- -D warnings ;;
    "")
        echo "merge-gate: the merge touches no crate — nothing to compile" ;;
    *)
        rev=$("$here/ci-reverse-deps.sh" susi-vendor-fastembed)
        if comm -12 <(sort <<<"$pkgs") <(sort <<<"$rev") | grep -q .; then
            "$here/ci-prefetch-onnxruntime.sh" || exit 1
        fi
        # shellcheck disable=SC2086
        cargo clippy$(printf ' -p %s' $pkgs) --all-targets --locked -- -D warnings ;;
esac
