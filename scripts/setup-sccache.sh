#!/usr/bin/env bash
# One compiler cache for every worktree's own cargo target on this machine.
#
#   scripts/setup-sccache.sh             enable it (sccache must be on PATH)
#   scripts/setup-sccache.sh --install   install sccache first (scoop, brew,
#                                        apt's cargo, or `cargo install`)
#   scripts/setup-sccache.sh --status    hit rate and cache size
#   scripts/setup-sccache.sh --off       remove what this script wrote
#
# Each worktree builds into a target of its own, copied from a snapshot of
# origin/main's registry crates (scripts/seed-target.sh). What the snapshot
# cannot hold still compiles in every new worktree: the vendored path patches
# (vendor/reqwest, vendor/esaxx-rs) and everything above them (ra2a, rmcp,
# tokenizers, fastembed, the candle crates: 100-150 s per worktree), and any
# registry crate a `-p <crate>` gate unifies with features the snapshot's
# whole-workspace build did not (tower, axum, tokio-stream and their
# dependents, measured). sccache keys a compile by the compiler, its
# arguments, the sources and the environment, so a crate one worktree
# compiled is served to every other from a local disk cache instead of
# compiled again. Workspace crates are passed through: cargo builds them
# incrementally, which sccache does not cache, and they are this branch's
# own code anyway.
#
# Machine-level, by design: it writes a marked block into the user's cargo
# config ($CARGO_HOME/config.toml, ~/.cargo/config.toml) naming the wrapper
# and the cache's place and size, never into the repository's .cargo/config
# (CI sets RUSTC_WRAPPER itself where it wants sccache, a cloud session has no
# sccache, and tests/ci_workflow_perf.rs pins that tracked config embeds no
# machine path). The cache lives beside the snapshot, outside every worktree
# (<primary>/../.sccache), so no checkout's cleanup removes it and the same
# real-time-scanning exclusion covers it. SUSI_SCCACHE_DIR and
# SUSI_SCCACHE_SIZE override the place and the size (default 50G).
#
# A config that already has a [build] or [env] table this script did not
# write is left alone with the lines to add printed: a second table of the
# same name is a TOML error that would break every cargo on the machine.
set -euo pipefail
[ ! -f "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh" ] || . "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh"

action=enable
case "${1:-}" in
"") ;;
--install) action=install ;;
--status) action=status ;;
--off) action=off ;;
*)
    echo "usage: setup-sccache.sh [--install | --status | --off]" >&2
    exit 2
    ;;
esac

cargo_home=${CARGO_HOME:-$HOME/.cargo}
config=$cargo_home/config.toml
begin='# >>> susi sccache (scripts/setup-sccache.sh) >>>'
end='# <<< susi sccache <<<'

# The cache's place: beside the clone's snapshot, outside every worktree.
cache_dir() {
    local common
    if [ -n "${SUSI_SCCACHE_DIR:-}" ]; then
        printf '%s\n' "$SUSI_SCCACHE_DIR"
        return
    fi
    common=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || common=$PWD/.git
    common=${common%/.git}
    printf '%s/.sccache\n' "$(dirname "$common")"
}

# A path as cargo's [env] table should carry it: native on Windows, with
# forward slashes (a backslash is an escape in a TOML basic string).
native_path() {
    case "${OSTYPE:-}" in
    msys* | cygwin*) cygpath -am "$1" 2>/dev/null || printf '%s\n' "$1" ;;
    *) printf '%s\n' "$1" ;;
    esac
}

written_block() {
    [ -f "$config" ] && grep -qF "$begin" "$config"
}

remove_block() {
    written_block || return 0
    local tmp
    tmp=$(mktemp)
    awk -v b="$begin" -v e="$end" '
        $0 == b { skip = 1; next }
        $0 == e { skip = 0; next }
        !skip { print }
    ' "$config" >"$tmp"
    # Nothing but the block: no config at all is the tidier state.
    if [ -z "$(tr -d '[:space:]' <"$tmp")" ]; then
        rm -f "$tmp" "$config"
    else
        mv "$tmp" "$config"
    fi
}

block() {
    local dir=$1 size=$2
    cat <<EOF
$begin
# Every cargo on this machine compiles through sccache: a crate one worktree
# compiled is served to every other. scripts/setup-sccache.sh --off removes
# this; --status shows the hit rate.
[build]
rustc-wrapper = "sccache"

[env]
SCCACHE_DIR = "$dir"
SCCACHE_CACHE_SIZE = "$size"
$end
EOF
}

install_sccache() {
    command -v sccache >/dev/null 2>&1 && return 0
    if command -v scoop >/dev/null 2>&1; then
        scoop install sccache
    elif command -v brew >/dev/null 2>&1; then
        brew install sccache
    elif command -v cargo >/dev/null 2>&1; then
        cargo install sccache --locked
    else
        echo "setup-sccache: no scoop, brew or cargo to install sccache with" >&2
        return 1
    fi
    command -v sccache >/dev/null 2>&1 || {
        echo "setup-sccache: sccache was installed but is not on PATH; open a new shell" >&2
        return 1
    }
}

case "$action" in
status)
    command -v sccache >/dev/null 2>&1 || {
        echo "sccache is not installed (scripts/setup-sccache.sh --install)"
        exit 0
    }
    if written_block; then
        echo "enabled in $config"
    else
        echo "not enabled in $config (scripts/setup-sccache.sh)"
    fi
    sccache --show-stats
    exit 0
    ;;
off)
    remove_block
    sccache --stop-server >/dev/null 2>&1 || true
    echo "setup-sccache: removed the sccache block from $config"
    exit 0
    ;;
install)
    install_sccache
    ;;
enable)
    command -v sccache >/dev/null 2>&1 || {
        echo "setup-sccache: sccache is not on PATH; scripts/setup-sccache.sh --install installs it" >&2
        exit 1
    }
    ;;
esac

dir=$(cache_dir)
mkdir -p "$dir"
dir_native=$(native_path "$dir")
size=${SUSI_SCCACHE_SIZE:-50G}

if written_block; then
    # Same place and size: nothing to do, quietly. Otherwise rewrite the block.
    if grep -qF "SCCACHE_DIR = \"$dir_native\"" "$config" && grep -qF "SCCACHE_CACHE_SIZE = \"$size\"" "$config"; then
        exit 0
    fi
    remove_block
fi
if [ -f "$config" ] && grep -qE '^\[(build|env)\]' "$config"; then
    cat >&2 <<EOF
setup-sccache: $config already has a [build] or [env] table; add these lines to them by hand:
  [build]  rustc-wrapper = "sccache"
  [env]    SCCACHE_DIR = "$dir_native"
           SCCACHE_CACHE_SIZE = "$size"
EOF
    exit 1
fi
mkdir -p "$cargo_home"
{
    [ ! -s "$config" ] || { cat "$config"; echo; }
    block "$dir_native" "$size"
} >"$config.tmp"
mv "$config.tmp" "$config"
# A server already running was started with some other cache; the next cargo
# starts one with this one.
sccache --stop-server >/dev/null 2>&1 || true
echo "setup-sccache: every cargo on this machine now compiles through sccache (cache $dir_native, $size); --status shows the hit rate, --off removes it"
