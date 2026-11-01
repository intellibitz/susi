#!/usr/bin/env bash
# Execute the affected test selection produced by ci-changed-crates.sh --plan.
# The live-service e2e has a separate provisioned CI job.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
cd "$root"
plan=${SUSI_BRANCH_TEST_PLAN:-}
if [ "$plan" = ALL ]; then
    SUSI_HERMETIC_RUNNER=nextest SUSI_HERMETIC_EXCLUDE_LIVE=1 \
        "$root/scripts/check-hermetic-tests.sh" --workspace
    exit
fi
pkgs=() root_tests=() rootpkg=
while IFS=: read -r kind value; do
    case "$kind" in
        test) [ -n "$value" ] || { echo 'empty test package' >&2; exit 1; }; pkgs+=("$value") ;;
        root-test) [ -n "$value" ] || { echo 'empty root test target' >&2; exit 1; }; root_tests+=("$value") ;;
        root-package) [ -n "$value" ] || { echo 'empty root package' >&2; exit 1; }; rootpkg=$value ;;
        lint|'') ;;
        *) echo "unknown branch test plan entry: $kind" >&2; exit 1 ;;
    esac
done <<<"$plan"
[ "${#pkgs[@]}" -gt 0 ] || { echo 'No affected crate tests.'; exit 0; }
other=() has_root=0
for pkg in "${pkgs[@]}"; do
    if [ "$pkg" = "$rootpkg" ]; then has_root=1; else other+=(-p "$pkg"); fi
done
run_tests() {
    SUSI_HERMETIC_RUNNER=nextest SUSI_HERMETIC_EXCLUDE_LIVE=1 \
        "$root/scripts/check-hermetic-tests.sh" "$@"
}
if [ "${#other[@]}" -gt 0 ]; then run_tests "${other[@]}"; fi
if [ "$has_root" -eq 1 ]; then
    if [ "${#root_tests[@]}" -gt 0 ]; then
        args=(-p "$rootpkg")
        for target in "${root_tests[@]}"; do args+=(--test "$target"); done
        run_tests "${args[@]}"
    else
        run_tests -p "$rootpkg" --lib --bins
    fi
fi
