#!/usr/bin/env bash
# Vendored-copy drift check.
#
# The 0-dependency refactor keeps byte-identical copies of shared modules in
# every consumer crate:
#   - crates/*/src/susi_core/<f>    vs crates/susi-core/vendor_template/susi_core/<f>
#   - crates/*/src/susi_sandbox/<f> vs crates/susi-sandbox/vendor_template/susi_sandbox/<f>
#   - crates/*/src/susi_native/<f>  vs crates/susi-native/vendor_template/susi_native/<f>
#   - flat leaf modules (susi_error.rs, susi_paths.rs, susi_config.rs): all
#     copies identical to each other (any copy may be the edit source).
#
# Nothing else enforces this invariant — a canonical edit without re-mirroring
# compiles and passes tests while silently forking the shared contract.
set -u
cd "$(dirname "$0")/.."
fail=0

check_template() {
    local template_dir="$1" module="$2"
    local t rel tm cm
    while IFS= read -r -d '' t; do
        rel="${t#"$template_dir"/}"
        tm=$(md5sum "$t" | cut -d' ' -f1)
        for c in crates/*/src/"$module"/"$rel"; do
            [ -f "$c" ] || continue
            cm=$(md5sum "$c" | cut -d' ' -f1)
            if [ "$tm" != "$cm" ]; then
                echo "DRIFT: $c != $t"
                fail=1
            fi
        done
    done < <(find "$template_dir" -name '*.rs' -print0)
}

check_template "crates/susi-core/vendor_template/susi_core" "susi_core"
check_template "crates/susi-sandbox/vendor_template/susi_sandbox" "susi_sandbox"
check_template "crates/susi-native/vendor_template/susi_native" "susi_native"

# Canonical -> template for susi_core only: susi_core copies are byte-
# identical to crates/susi-core/src (vendor_sync_tests enforces the same).
# Drift here previously went unnoticed when the canonical was reformatted
# after copies were last mirrored. susi_sandbox / susi_native are excluded
# on purpose: their canonical files are the real implementations while
# vendored copies are adapted variants (crate:: paths that cannot exist
# inside the implementing crate).
check_canonical() {
    local template_dir="$1" canonical_dir="$2"
    local t rel tm cm
    while IFS= read -r -d '' t; do
        rel="${t#"$template_dir"/}"
        [ -f "$canonical_dir/$rel" ] || continue
        tm=$(md5sum "$t" | cut -d' ' -f1)
        cm=$(md5sum "$canonical_dir/$rel" | cut -d' ' -f1)
        if [ "$tm" != "$cm" ]; then
            echo "DRIFT: canonical $canonical_dir/$rel != template $t"
            fail=1
        fi
    done < <(find "$template_dir" -name '*.rs' -print0)
}

check_canonical "crates/susi-core/vendor_template/susi_core" "crates/susi-core/src"

# Flat leaf modules: every copy identical to every other copy.
for leaf in susi_error susi_paths susi_config; do
    ref=""
    ref_md5=""
    for c in crates/*/src/$leaf.rs; do
        [ -f "$c" ] || continue
        cm=$(md5sum "$c" | cut -d' ' -f1)
        if [ -z "$ref_md5" ]; then
            ref="$c"
            ref_md5="$cm"
        elif [ "$cm" != "$ref_md5" ]; then
            echo "DRIFT: $c != $ref"
            fail=1
        fi
    done
done

# Consumers must not vendor files the template doesn't ship (dead copies).
for t in susi_core susi_sandbox susi_native; do
    case "$t" in
        susi_core) template_dir="crates/susi-core/vendor_template/susi_core" ;;
        susi_sandbox) template_dir="crates/susi-sandbox/vendor_template/susi_sandbox" ;;
        susi_native) template_dir="crates/susi-native/vendor_template/susi_native" ;;
    esac
    for d in crates/*/src/$t; do
        [ -d "$d" ] || continue
        while IFS= read -r -d '' f; do
            rel="${f#"$d"/}"
            if [ ! -f "$template_dir/$rel" ]; then
                echo "ORPHAN: $f (no template counterpart at $template_dir/$rel)"
                fail=1
            fi
        done < <(find "$d" -name '*.rs' -print0)
    done
done

if [ "$fail" -eq 0 ]; then
    echo "vendored copies in sync"
fi
exit $fail
