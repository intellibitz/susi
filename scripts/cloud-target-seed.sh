#!/usr/bin/env bash
# Seed a cloud session's cargo target from the snapshot a main workflow publishes.
#
# A cloud container starts with no target/: every registry and git crate of the
# dependency tree compiled cold (about 15 minutes on 4 vCPU before the first
# `susi workflow check`), and the gate's clippy of a changed crate's dependents
# check-built them again (210 s measured; docs/workflow-speed.md). A laptop's
# worktrees copy a warm snapshot from the primary checkout (scripts/seed-target.sh);
# a container has no primary. So `.github/workflows/target-seed.yml` builds
# origin/main the way a cloud session does — the same toolchain, the same
# `cargo` configuration scripts/cloud-onnxruntime.sh writes — strips the
# workspace's own units (`susi workflow seed-target --export`), and publishes the
# rest as assets of the rolling `target-seed` pre-release, named by a key of
# everything the kept units' fingerprints depend on. This script, run by the
# session-start hook before the first build, fetches the asset for this
# checkout's key (or the newest one, when no snapshot for this exact key is
# published yet: cargo recompiles the few crates that differ and keeps the
# rest), verifies it, and unpacks it into target/. A target/ that already holds
# a build is left alone, as scripts/seed-target.sh leaves one.
#
#   scripts/cloud-target-seed.sh          seed ./target (a no-op once it holds a build)
#   scripts/cloud-target-seed.sh --key    print this checkout's snapshot key
#   scripts/cloud-target-seed.sh --name   print the asset name: linux-<arch>-<key>
#   scripts/cloud-target-seed.sh --url    print where the assets are fetched from
#   scripts/cloud-target-seed.sh --wait   wait for a seeding or build the hook started
#
# Environment:
#   SUSI_TARGET_SEED_URL   where the assets are (default: the target-seed release
#                          of this repository's origin on GitHub); file:// works
#   SUSI_CLOUD_SEED=0      do nothing (SUSI_NO_SEED=1 too, as for every seeding)
#   SUSI_TARGET_SEED_KEEP_TMP=1  keep the download directory on failure
#
# The key: the external packages of Cargo.lock (the path packages are stripped
# from the snapshot anyway), the toolchain pin, `rustc -vV`, the machine, the
# dev profile tables of Cargo.toml and the cargo configuration the cloud adds
# for ONNX Runtime (its rpath is in every unit's rustflags). The same script
# computes it in the workflow and here, so the two cannot drift.
set -uo pipefail
root=$(git rev-parse --show-toplevel 2>/dev/null) || root=$(pwd)
cd "$root" || exit 0
me=$(basename "$0")

say() { echo "cloud-target-seed: $*" >&2; }

key() {
    {
        echo "susi-target-seed-v1"
        # Only packages with a source: path packages are the workspace's own.
        awk '
            /^\[\[package\]\]$/ { if (has_source) print block; block = ""; has_source = 0; next }
            /^\[/ { if (has_source) print block; block = ""; has_source = 0; in_other = 1 }
            { if ($0 ~ /^source = /) has_source = 1; block = block "\n" $0 }
            END { if (has_source) print block }
        ' Cargo.lock 2>/dev/null
        cat rust-toolchain.toml rust-toolchain 2>/dev/null
        rustc -vV 2>/dev/null
        uname -m
        # The dev profile shapes every unit (opt-level, debuginfo); the version
        # line and the rest of the manifest do not.
        awk '/^\[profile\.dev/ { on = 1 } /^\[/ && !/^\[profile\.dev/ { on = 0 } on' Cargo.toml 2>/dev/null
        [ ! -x scripts/cloud-onnxruntime.sh ] || scripts/cloud-onnxruntime.sh --print-config 2>/dev/null
    } | sha256sum | cut -c1-16
}

arch_name() {
    case "$(uname -m)" in
    x86_64) echo x86_64 ;;
    aarch64 | arm64) echo aarch64 ;;
    *) uname -m ;;
    esac
}

name() { echo "linux-$(arch_name)-$(key)"; }

default_url() {
    local origin slug
    origin=$(git remote get-url origin 2>/dev/null || true)
    slug=$(printf '%s' "$origin" | sed -nE 's#^(https://github\.com/|git@github\.com:)([^/]+/[^/]+)/?$#\2#p')
    # A clone URL may end in .git (the harness's does not; a hand clone's may):
    # the release URL never does. sed -E has no lazy quantifier to take it off.
    slug=${slug%.git}
    [ -n "$slug" ] || slug=intellibitz/susi
    echo "https://github.com/$slug/releases/download/target-seed"
}

lock_path() { echo "$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null || echo .git)/susi-cloud-build.lock"; }

mode=${1:-seed}
case "$mode" in
--key) key; exit 0 ;;
--name) name; exit 0 ;;
--url) echo "${SUSI_TARGET_SEED_URL:-$(default_url)}"; exit 0 ;;
--wait)
    # The hook's background subshell holds this lock through the seeding and the
    # build it starts; waiting on it is how a session waits for both.
    lock=$(lock_path)
    [ -f "$lock" ] || exit 0
    exec 9>"$lock"
    flock 9
    exit 0
    ;;
-h | --help)
    sed -n '2,36p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
seed) ;;
*)
    echo "usage: $me [--key | --name | --url | --wait]" >&2
    exit 2
    ;;
esac

if [ "${SUSI_CLOUD_SEED:-1}" = 0 ] || [ "${SUSI_NO_SEED:-0}" = 1 ]; then
    say "skipped (SUSI_CLOUD_SEED=0 or SUSI_NO_SEED=1)"
    exit 0
fi
case "$(uname -s)" in
Linux) ;;
*) say "Linux only (this host is $(uname -s))"; exit 0 ;;
esac
if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    say "CARGO_TARGET_DIR is set; cargo would not build in target/, so nothing is seeded"
    exit 0
fi
target="$root/target"
if [ -L "$target" ]; then
    say "target is a link; leaving it"
    exit 0
fi
if [ -d "$target" ] && [ -n "$(ls -A "$target" 2>/dev/null)" ]; then
    say "target already holds a build; nothing to seed"
    exit 0
fi
for tool in curl tar gzip sha256sum; do
    command -v "$tool" >/dev/null 2>&1 || { say "no $tool here; building cold"; exit 0; }
done

base=${SUSI_TARGET_SEED_URL:-$(default_url)}
base=${base%/}
want=$(name)
latest="linux-$(arch_name)-latest"
tmp=$(mktemp -d "$root/target.seed.XXXXXX") || exit 0
cleanup() {
    if [ "${SUSI_TARGET_SEED_KEEP_TMP:-0}" != 1 ]; then rm -rf "$tmp"; fi
}
trap cleanup EXIT

fetch() { curl -sSfL --retry 3 --retry-delay 2 -o "$2" "$1"; }

started=$SECONDS
manifest="$tmp/manifest"
if fetch "$base/$want.manifest" "$manifest" 2>/dev/null; then
    say "snapshot for this key: $want"
elif fetch "$base/$latest.manifest" "$manifest" 2>/dev/null; then
    say "no snapshot for key ${want#linux-*-} yet; taking the newest ($latest): cargo recompiles what differs"
else
    say "no snapshot published at $base; building cold"
    exit 0
fi
# The manifest is sha256sum's own format: "<sha256>  <part>", one part per line,
# the parts in order. A part is a slice of one gzip'd tar of the snapshot.
parts=()
while read -r sum part; do
    [ -n "$sum" ] && [ -n "$part" ] || continue
    case "$part" in */* | *..* | "") say "refusing a manifest naming $part"; exit 0 ;; esac
    parts+=("$part")
done <"$manifest"
[ "${#parts[@]}" -gt 0 ] || { say "the manifest names no parts; building cold"; exit 0; }
for part in "${parts[@]}"; do
    if ! fetch "$base/$part" "$tmp/$part"; then
        say "could not fetch $part; building cold"
        exit 0
    fi
done
if ! (cd "$tmp" && sha256sum --quiet -c manifest >/dev/null 2>&1); then
    say "a part does not match its checksum; building cold"
    exit 0
fi
staging="$tmp/target"
mkdir -p "$staging"
if ! cat "${parts[@]/#/$tmp/}" | tar -xzf - -C "$staging"; then
    say "could not unpack the snapshot; building cold"
    exit 0
fi
[ -f "$staging/.susi-seed" ] || { say "the archive is not a snapshot (no .susi-seed); building cold"; exit 0; }
# Someone (a hand-run cargo) may have started building meanwhile.
if [ -d "$target" ] && [ -n "$(ls -A "$target" 2>/dev/null)" ]; then
    say "target started holding a build during the download; leaving it"
    exit 0
fi
rmdir "$target" 2>/dev/null || true
if ! mv "$staging" "$target"; then
    say "could not move the snapshot into place; building cold"
    exit 0
fi
printf 'seeded from %s/%s at %s\n' "$base" "${parts[0]%.part*}" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >"$target/.susi-seeded"
say "seeded target from $(head -1 "$target/.susi-seed" | cut -c1-12) in $((SECONDS - started))s (${#parts[@]} part(s)); the first build compiles the workspace crates only"
exit 0
