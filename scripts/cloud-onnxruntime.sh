#!/usr/bin/env bash
# Give a cloud session the ONNX Runtime its build needs, from a host it can reach.
#
# `susi-vendor-fastembed` builds `ort-sys`, whose build script downloads a
# prebuilt ONNX Runtime from pyke's CDN (cdn.pyke.io) — one unretried GET,
# cached under ~/.cache/ort.pyke.io afterwards. A cloud container's egress
# policy allows GitHub and the package registries and refuses that CDN
# ("CONNECT proxy failed: proxy server responded 403"), so every cold build of
# susi in a cloud session died in ort-sys before a single workspace crate
# compiled. The same ONNX Runtime release (`ms@<version>` on the CDN is
# Microsoft's own build) is published on GitHub, which the proxy allows.
#
# So, for the cloud: fetch Microsoft's release once, verify its checksum, and
# point ort-sys at it through cargo's own configuration — `ORT_LIB_LOCATION`
# (the directory holding `libonnxruntime.so`) and `ORT_PREFER_DYNAMIC_LINK=1`
# (the release ships a shared library, and ort-sys links a user-provided
# library statically unless told otherwise), plus an rpath so the binary runs
# from anywhere (hooks, tests, `target/debug/susi` by hand) without an
# `LD_LIBRARY_PATH`. The configuration goes into `$CARGO_HOME/config.toml` —
# never the repository's `.cargo/config.toml`, which every laptop shares and
# `tests/ci_workflow_perf.rs` pins — between two marker lines, so a re-run
# replaces its own block and nothing else.
#
#   scripts/cloud-onnxruntime.sh            install (idempotent; a no-op once done)
#   scripts/cloud-onnxruntime.sh --check    exit 0 when the library and the cargo
#                                           configuration are in place
#   scripts/cloud-onnxruntime.sh --print-config
#                                           print the cargo configuration block
#
# Environment:
#   SUSI_ONNXRUNTIME_VERSION   ONNX Runtime release (default: the one ort-sys in
#                              Cargo.lock expects; see VERSIONS below)
#   SUSI_ONNXRUNTIME_DIR       where releases are unpacked
#                              (default: ~/.cache/susi/onnxruntime)
#   SUSI_ONNXRUNTIME_SHA256    checksum of the tarball, for a version/arch this
#                              script does not pin (an unpinned download is refused)
#   SUSI_ONNXRUNTIME_URL       the tarball's URL (default: the GitHub release asset)
#   CARGO_HOME                 where cargo reads config.toml (default: ~/.cargo)
#
# The session-start hook runs this in a cloud session (scripts/workflow-session-
# start.sh); anywhere else it is a deliberate choice, run by hand. Linux only:
# the cloud containers are.
set -uo pipefail

# ort-sys pins one ONNX Runtime per release; the two move together. Bump this
# table with `ort-sys` in Cargo.lock (the version the CDN URL names, `ms@…`).
#   ort-sys 2.0.0-rc.13  ->  ONNX Runtime 1.28.0
default_version=1.28.0
# sha256 of onnxruntime-linux-<arch>-<version>.tgz, as published on GitHub.
pinned_sha256() {
    case "$1" in
    x64-1.28.0) echo a3e1b79d7bb1bf09696ce675f49e4064e6c81f6202b8225624fff0e93f8d6407 ;;
    *) echo "" ;;
    esac
}

mode=${1:-install}
case "$mode" in
install | --check | --print-config) ;;
-h | --help)
    sed -n '2,42p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
*)
    echo "usage: cloud-onnxruntime.sh [--check | --print-config]" >&2
    exit 2
    ;;
esac

case "$(uname -s)" in
Linux) ;;
*)
    echo "cloud-onnxruntime: Linux only (this host is $(uname -s))" >&2
    exit 1
    ;;
esac
case "$(uname -m)" in
x86_64) arch=x64 triple=x86_64-unknown-linux-gnu ;;
aarch64 | arm64) arch=aarch64 triple=aarch64-unknown-linux-gnu ;;
*)
    echo "cloud-onnxruntime: no ONNX Runtime release for $(uname -m)" >&2
    exit 1
    ;;
esac

version=${SUSI_ONNXRUNTIME_VERSION:-$default_version}
name="onnxruntime-linux-$arch-$version"
base=${SUSI_ONNXRUNTIME_DIR:-$HOME/.cache/susi/onnxruntime}
dest="$base/$name"
lib="$dest/lib"
url=${SUSI_ONNXRUNTIME_URL:-"https://github.com/microsoft/onnxruntime/releases/download/v$version/$name.tgz"}
cargo_home=${CARGO_HOME:-$HOME/.cargo}
config="$cargo_home/config.toml"
begin='# >>> susi cloud onnxruntime (scripts/cloud-onnxruntime.sh) >>>'
end='# <<< susi cloud onnxruntime <<<'

config_block() {
    cat <<EOF
$begin
# ort-sys cannot reach pyke's CDN from a cloud container; link the ONNX Runtime
# release fetched from GitHub instead, and bake its path into every binary.
[env]
ORT_LIB_LOCATION = "$lib"
ORT_PREFER_DYNAMIC_LINK = "1"

[target.$triple]
rustflags = ["-C", "link-arg=-Wl,-rpath,$lib"]
$end
EOF
}

installed() { [ -f "$lib/libonnxruntime.so" ]; }
configured() { [ -f "$config" ] && grep -qF "ORT_LIB_LOCATION = \"$lib\"" "$config"; }

case "$mode" in
--print-config)
    config_block
    exit 0
    ;;
--check)
    if installed && configured; then
        echo "onnxruntime $version at $lib; cargo config in $config"
        exit 0
    fi
    installed || echo "onnxruntime $version is not installed at $lib" >&2
    configured || echo "$config does not point cargo at $lib" >&2
    exit 1
    ;;
esac

# 1. The library.
if installed; then
    echo "onnxruntime $version already at $lib"
else
    for tool in curl tar sha256sum; do
        command -v "$tool" >/dev/null 2>&1 || {
            echo "cloud-onnxruntime: $tool is required" >&2
            exit 1
        }
    done
    expected=${SUSI_ONNXRUNTIME_SHA256:-$(pinned_sha256 "$arch-$version")}
    if [ -z "$expected" ]; then
        echo "cloud-onnxruntime: no pinned checksum for $name; set SUSI_ONNXRUNTIME_SHA256 to the tarball's sha256 (an unverified download is refused)" >&2
        exit 1
    fi
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/susi-onnxruntime.XXXXXX") || exit 1
    trap 'rm -rf "$tmp"' EXIT
    echo "fetching $url"
    if ! curl -fsSL --retry 3 --retry-delay 2 --max-time 300 -o "$tmp/$name.tgz" "$url"; then
        echo "cloud-onnxruntime: could not download $url (is github.com allowed by this session's network policy?)" >&2
        exit 1
    fi
    actual=$(sha256sum "$tmp/$name.tgz" | cut -d' ' -f1)
    if [ "$actual" != "$expected" ]; then
        echo "cloud-onnxruntime: checksum mismatch for $name.tgz: got $actual, expected $expected — refusing to install it" >&2
        exit 1
    fi
    mkdir -p "$base" || exit 1
    tar -xzf "$tmp/$name.tgz" -C "$tmp" || exit 1
    [ -d "$tmp/$name/lib" ] || {
        echo "cloud-onnxruntime: $name.tgz has no lib/ directory" >&2
        exit 1
    }
    rm -rf "$dest.partial"
    mv "$tmp/$name" "$dest.partial" && mv "$dest.partial" "$dest" || exit 1
    # The release ships libonnxruntime.so.<version> and .so.1 only; the linker
    # resolves `-lonnxruntime` through the unversioned name.
    (cd "$lib" && ln -sfn "libonnxruntime.so.$version" libonnxruntime.so) || exit 1
    echo "installed onnxruntime $version at $lib (sha256 verified)"
fi

# 2. Cargo's configuration: our block, replaced in place when it is there.
mkdir -p "$cargo_home" || exit 1
if [ -f "$config" ]; then
    # Without our block, a foreign [env] or [target.<triple>] table would be
    # duplicated by appending ours, which is not valid TOML: say what to add.
    if ! grep -qF "$begin" "$config" && grep -qE "^\[(env|target\.$triple)\]" "$config"; then
        echo "cloud-onnxruntime: $config already has an [env] or [target.$triple] table; merge this into it by hand:" >&2
        config_block >&2
        exit 3
    fi
    tmpcfg=$(mktemp "${TMPDIR:-/tmp}/susi-cargo-config.XXXXXX") || exit 1
    awk -v b="$begin" -v e="$end" '
        $0 == b { skip = 1; next }
        $0 == e { skip = 0; next }
        !skip { print }
    ' "$config" >"$tmpcfg" && {
        [ -s "$tmpcfg" ] && [ -n "$(tail -c1 "$tmpcfg")" ] && echo >>"$tmpcfg"
        config_block >>"$tmpcfg"
    } && mv "$tmpcfg" "$config" || exit 1
else
    config_block >"$config" || exit 1
fi
echo "cargo config: $config points ort-sys at $lib (ORT_LIB_LOCATION, ORT_PREFER_DYNAMIC_LINK, rpath)"
echo "note: a target/ built before this was configured is rebuilt by the next cargo (the rustflags changed)"
exit 0
