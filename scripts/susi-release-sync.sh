#!/usr/bin/env bash
# Promote the newest tagged susi release to this host.
#
# The local susi (~/.susi/bin/susi and the daemon it runs) is a release-only
# toolchain: it builds future susi, so dev builds never touch it (`cargo xb`
# builds without installing). This script is the one path that updates it:
#
#   1. resolve the newest vX.Y.Z tag on the release remote
#   2. take the tag's published binary for this host when the build would
#      carry no accelerator (the CI leg built exactly that), else build the
#      tag natively for this host's GPU in a private checkout
#      (~/.susi/release-src) with a private target dir — never the dev tree
#   3. verify the binary reports the tag's version
#   4. wait until no susi client is mid-mission, then swap it in atomically,
#      keeping the previous binary as susi.prev
#   5. restart the daemon on the new binary; roll back if it fails health
#
# A GPU host builds locally rather than downloading the CI asset because the CI
# CUDA binary targets compute capability 7.5 (no GPU on the runner), which drops
# candle's sm_80+ kernels (bf16); a native build targets this GPU exactly. A
# host without an accelerator gets the same binary either way, and the download
# is the cheaper path by an order of magnitude (2026-10-31, Windows, 28 cores:
# native CPU build of v0.21.28 12+ min; the 88 MB asset about one).
# SUSI_RELEASE_BUILD=1 insists on the build.
#
# Usage:
#   susi-release-sync.sh                 sync to the newest release if newer
#   susi-release-sync.sh --check         report installed vs newest; no changes
#   susi-release-sync.sh --tag vX.Y.Z    install a specific release (rollback/pin)
#   susi-release-sync.sh --rollback      restore susi.prev
#   susi-release-sync.sh --install-timer   install + enable the systemd user timer
#   susi-release-sync.sh --uninstall-timer remove the timer
#
# Environment:
#   SUSI_RELEASE_REMOTE    git URL of the release repo (default: GitHub intellibitz/susi)
#   SUSI_RELEASE_SRC       private release checkout (default: ~/.susi/release-src)
#   SUSI_RELEASE_TARGET    private cargo target dir (default: ~/.susi/build-cache/release-sync)
#   SUSI_RELEASE_FEATURES  cargo features override (default: detected: cuda / metal / none)
#   SUSI_RELEASE_IDLE_WAIT seconds to wait for an idle susi before swapping (default: 1800)
#
# Exit codes: 0 up to date or installed; 1 failure; 75 deferred (susi busy —
# the timer retries, and the cached build makes the retry cheap).

set -euo pipefail
[ ! -f "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh" ] || . "$(dirname "${BASH_SOURCE[0]}")/posix-compat.sh"

SUSI_HOME="${HOME}/.susi"
case "$SUSI_HOME" in /* | [A-Za-z]:[\\/]*) ;; *) echo 'SUSI_HOME must be absolute' >&2; exit 1 ;; esac
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) SUSI_HOME="$(cygpath -aw "$SUSI_HOME")" ;; esac
export SUSI_HOME
BIN_DIR="${SUSI_HOME}/bin"
BIN="${BIN_DIR}/susi"
WINDOWS=0
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) WINDOWS=1; BIN="${BIN}.exe" ;; esac
PREV="${BIN}.prev"
[ "$WINDOWS" != 1 ] || PREV="${BIN_DIR}/susi.prev.exe"
WINDOWS_DAEMON_WAS_RUNNING=""
WINDOWS_HELPER="$(dirname "${BASH_SOURCE[0]}")/susi-release-sync-windows.ps1"
windows_release() {
    local action="$1"; shift
    local args=(-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$(cygpath -w "$WINDOWS_HELPER")" -Action "$action" -Bin "$(cygpath -w "$BIN")")
    [ $# -eq 0 ] || args+=(-Built "$(cygpath -w "$1")")
    powershell.exe "${args[@]}"
}
MARKER="${BIN_DIR}/susi.build.json"
SELF_INSTALLED="${BIN_DIR}/susi-release-sync"
REMOTE="${SUSI_RELEASE_REMOTE:-https://github.com/intellibitz/susi.git}"
SRC="${SUSI_RELEASE_SRC:-${SUSI_HOME}/release-src}"
TARGET="${SUSI_RELEASE_TARGET:-${SUSI_HOME}/build-cache/release-sync}"
IDLE_WAIT="${SUSI_RELEASE_IDLE_WAIT:-1800}"
UNIT_DIR="${XDG_CONFIG_HOME:-${HOME}/.config}/systemd/user"
UNIT=susi-release-sync
EX_TEMPFAIL=75

# systemd user units start with a minimal PATH; cargo and nvcc live elsewhere.
for d in "${HOME}/.cargo/bin" /opt/cuda/bin /usr/local/cuda/bin; do
    [ -d "$d" ] && case ":$PATH:" in *":$d:"*) ;; *) PATH="$d:$PATH" ;; esac
done
export PATH

log() { printf '[release-sync] %s\n' "$*" >&2; }
die() { log "error: $*"; exit 1; }

# ---- versions -------------------------------------------------------------

latest_tag() {
    git ls-remote --tags --refs "$REMOTE" 'v*' \
        | awk '{sub("refs/tags/", "", $2); print $2}' \
        | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' \
        | sort -V | tail -n 1
}

installed_version() {
    [ -x "$BIN" ] || return 0
    "$BIN" --version 2>/dev/null | awk '{print $2}' | head -n 1
}

installed_channel() {
    [ -f "$MARKER" ] || return 0
    sed -n 's/.*"channel":"\([a-z]*\)".*/\1/p' "$MARKER"
}

# 0 when $1 < $2 (semver, no pre-release parts)
version_lt() {
    [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n 1)" = "$1" ]
}

# ---- build ----------------------------------------------------------------

detect_features() {
    if [ -n "${SUSI_RELEASE_FEATURES+x}" ]; then
        printf '%s' "$SUSI_RELEASE_FEATURES"
        return
    fi
    case "$(uname -s)" in
        Darwin) printf 'metal' ;;
        Linux)
            if command -v nvcc >/dev/null 2>&1 && command -v nvidia-smi >/dev/null 2>&1 \
                && nvidia-smi --query-gpu=name --format=csv,noheader >/dev/null 2>&1; then
                printf 'cuda'
            fi
            ;;
        *) ;;
    esac
}

checkout_tag() {
    local tag="$1"
    if [ ! -d "$SRC/.git" ]; then
        log "cloning $REMOTE into $SRC"
        git clone --quiet --no-checkout "$REMOTE" "$SRC"
    fi
    git -C "$SRC" remote set-url origin "$REMOTE"
    git -C "$SRC" fetch --quiet --force --prune origin "refs/tags/${tag}:refs/tags/${tag}"
    git -C "$SRC" -c advice.detachedHead=false checkout --quiet --force --detach "refs/tags/${tag}"
    git -C "$SRC" clean --quiet -ffdx
}

build_tag() {
    local tag="$1" features="$2"
    local args=(build --release --locked -p susi --bin susi)
    [ -n "$features" ] && args+=(--features "$features")

    if [ "$features" = cuda ]; then
        # Target this GPU exactly (candle-kernels reads CUDA_COMPUTE_CAP).
        local cap
        cap="$(nvidia-smi --query-gpu=compute_cap --format=csv,noheader 2>/dev/null | head -n 1 | tr -d '. ')"
        [ -n "$cap" ] && export CUDA_COMPUTE_CAP="$cap"
        # cudarc pins a CUDA allowlist that tops out at 13.3; newer 13.x is
        # ABI-compatible, so clamp (same rule as xtask and install.sh).
        local ver major minor
        ver="$(nvcc --version | sed -n 's/.*release \([0-9]*\.[0-9]*\).*/\1/p')"
        major="${ver%%.*}" minor="${ver#*.}"
        if [ "$major" = 13 ] && [ "${minor:-0}" -gt 3 ]; then
            export CUDARC_CUDA_VERSION=13030
        fi
        log "cuda build: compute_cap=${CUDA_COMPUTE_CAP:-auto} toolkit=${ver}"
    fi

    log "building ${tag} (features: ${features:-none}) in ${SRC}"
    (cd "$SRC" && CARGO_TARGET_DIR="$TARGET" nice -n 10 cargo "${args[@]}")
}

# ---- download ---------------------------------------------------------------

# `owner/repo` of the release remote, for `gh release download`.
release_repo_slug() {
    printf '%s' "$REMOTE" | sed -E 's#^(https?://github\.com/|git@github\.com:)##; s#\.git$##'
}

# The published asset for this host when the build would carry no accelerator;
# empty when the host has no published leg, or when SUSI_RELEASE_BUILD=1.
release_asset_name() {
    [ -z "${SUSI_RELEASE_BUILD:-}" ] || return 0
    case "$(uname -s)/$(uname -m)" in
        MINGW*/x86_64 | MSYS*/x86_64 | CYGWIN*/x86_64) printf 'susi-windows-x86_64.exe' ;;
        Linux/x86_64) printf 'susi-linux-x86_64' ;;
        Linux/aarch64) printf 'susi-linux-aarch64' ;;
        Darwin/arm64) printf 'susi-macos-aarch64' ;;
        *) ;;
    esac
}

# Fetch the tag's published binary and its checksum into $TARGET/download and
# print the binary's path. Prints nothing when anything is missing or does not
# match, and the caller builds instead: the download is a shortcut, never a
# different outcome.
download_release_asset() {
    local tag="$1" asset="$2" dir="${TARGET}/download/${tag}"
    command -v gh >/dev/null 2>&1 || return 0
    rm -rf "$dir" && mkdir -p "$dir" || return 0
    if ! gh release download "$tag" --repo "$(release_repo_slug)" --dir "$dir" \
        --pattern "$asset" --pattern "${asset}.sha256" >/dev/null 2>&1; then
        log "no published ${asset} for ${tag}; building instead"
        return 0
    fi
    if [ ! -s "$dir/$asset" ] || [ ! -s "$dir/${asset}.sha256" ]; then
        log "incomplete download of ${asset}; building instead"
        return 0
    fi
    local want have
    want="$(awk '{print tolower($1)}' "$dir/${asset}.sha256")"
    have="$(sha256sum "$dir/$asset" | awk '{print tolower($1)}')"
    if [ -z "$want" ] || [ "$want" != "$have" ]; then
        log "checksum mismatch for ${asset}; building instead"
        return 0
    fi
    chmod 0755 "$dir/$asset"
    printf '%s' "$dir/$asset"
}

# ---- liveness ---------------------------------------------------------------

# PIDs of susi *clients* (anything but the daemon and its leaf services)
# running the installed binary — a mission, a shell, or `susi release`.
busy_clients() {
    if [ "$WINDOWS" = 1 ]; then windows_release busy; return; fi
    local p exe arg1
    for p in /proc/[0-9]*; do
        exe="$(readlink "$p/exe" 2>/dev/null)" || continue
        case "$exe" in "$BIN" | "$BIN (deleted)" | "${BIN_DIR}/susi.prev") ;; *) continue ;; esac
        arg1="$(tr '\0' '\n' <"$p/cmdline" 2>/dev/null | sed -n 2p)"
        case "$arg1" in daemon-start | service-run | --version) ;; *) printf '%s ' "${p#/proc/}" ;; esac
    done
}

wait_for_idle() {
    local deadline=$((SECONDS + IDLE_WAIT)) busy
    while true; do
        busy="$(busy_clients)" || die "cannot determine whether installed Susi is idle"
        [ -n "$busy" ] || break
        if [ "$SECONDS" -ge "$deadline" ]; then
            log "susi still busy (pids: ${busy}); deferring swap to the next run"
            exit "$EX_TEMPFAIL"
        fi
        log "susi busy (pids: ${busy}); waiting to swap"
        sleep 30
    done
}

daemon_running() {
    if [ "$WINDOWS" = 1 ]; then [ -n "$(windows_release running)" ]; return; fi
    local p
    for p in /proc/[0-9]*; do
        case "$(readlink "$p/exe" 2>/dev/null)" in "$BIN" | "$BIN (deleted)") ;; *) continue ;; esac
        [ "$(tr '\0' '\n' <"$p/cmdline" 2>/dev/null | sed -n 2p)" = daemon-start ] && return 0
    done
    return 1
}

health_url() {
    local offset="${SUSI_PORT_OFFSET:-}"
    if [ -z "$offset" ] && [ -f "${SUSI_HOME}/config.json" ]; then
        offset="$(sed -n 's/.*"port_offset"[[:space:]]*:[[:space:]]*\([0-9]*\).*/\1/p' "${SUSI_HOME}/config.json" | head -n 1)"
    fi
    # /health is the unauthenticated probe on the GEMI port (9091).
    printf 'http://127.0.0.1:%s/health' "$((9091 + ${offset:-0}))"
}

restart_service() {
    if systemctl --user is-active --quiet susi.service 2>/dev/null; then
        log "restarting always-on susi.service"
        systemctl --user restart susi.service
    elif [ -n "$WINDOWS_DAEMON_WAS_RUNNING" ] || daemon_running; then
        log "restarting daemon"
        "$BIN" restart >/dev/null || return 1
    else
        log "daemon not running; it starts on the new binary at next use"
        return 0
    fi
    local url i
    url="$(health_url)"
    for i in $(seq 1 60); do
        curl -fsS --max-time 2 "$url" >/dev/null 2>&1 && return 0
        sleep 1
    done
    log "daemon failed health check at ${url}"
    return 1
}

# ---- install ----------------------------------------------------------------

write_marker() {
    local version="$1" features="$2" commit="$3"
    local accel=null
    case "$features" in *cuda*) accel='"cuda"' ;; *metal*) accel='"metal"' ;; *mkl*) accel='"mkl"' ;; *) ;; esac
    printf '{"profile":"release","accelerator":%s,"channel":"release","version":"%s","commit":"%s","installed_at":%s}\n' \
        "$accel" "$version" "$commit" "$(date +%s)" >"${MARKER}.new"
    mv -f "${MARKER}.new" "$MARKER"
}

swap_in() {
    local built="$1"
    mkdir -p "$BIN_DIR"
    if [ "$WINDOWS" = 1 ]; then
        WINDOWS_DAEMON_WAS_RUNNING="$(windows_release running)"
        [ ! -f "$MARKER" ] || cp -p "$MARKER" "${MARKER}.prev"
        if ! windows_release swap "$built"; then
            # Restore service availability as well as bytes on a failed swap.
            restart_service || true
            return 1
        fi
        return
    fi
    install -m 0755 "$built" "${BIN}.new"
    if [ -x "$BIN" ]; then
        cp -p "$BIN" "$PREV"
        [ -f "$MARKER" ] && cp -p "$MARKER" "${MARKER}.prev"
    fi
    # rename(2): a running daemon keeps its old inode; new execs get the new one.
    mv -f "${BIN}.new" "$BIN"
}

rollback() {
    [ -x "$PREV" ] || die "no previous binary ($PREV) to roll back to"
    log "rolling back to $("$PREV" --version 2>/dev/null || echo "$PREV")"
    if [ "$WINDOWS" = 1 ]; then
        WINDOWS_DAEMON_WAS_RUNNING="${WINDOWS_DAEMON_WAS_RUNNING:-$(windows_release running)}"
        windows_release rollback || return 1
    else
        cp -p "$PREV" "${BIN}.new"
        mv -f "${BIN}.new" "$BIN"
    fi
    [ -f "${MARKER}.prev" ] && cp -p "${MARKER}.prev" "$MARKER"
    restart_service
}

refresh_self() {
    # Keep the timer's copy of this script at the installed release's version.
    local src="${SRC}/scripts/susi-release-sync.sh"
    if [ -f "$SELF_INSTALLED" ] && [ -f "$src" ]; then
        install -m 0755 "$src" "${SELF_INSTALLED}.new"
        mv -f "${SELF_INSTALLED}.new" "$SELF_INSTALLED"
        if [ "$WINDOWS" = 1 ]; then
            install -m 0644 "${SRC}/scripts/susi-release-sync-windows.ps1" "${BIN_DIR}/susi-release-sync-windows.ps1"
        fi
    fi
}

sync_release() {
    local tag="$1" explicit="$2"
    local want="${tag#v}" have channel
    have="$(installed_version)"
    channel="$(installed_channel)"

    if [ "$have" = "$want" ] && [ "$channel" = release ]; then
        log "up to date: susi ${have} (release)"
        return 0
    fi
    if [ -n "$have" ] && [ "$explicit" = 0 ] && version_lt "$want" "$have"; then
        log "installed susi ${have} is newer than release ${tag}; leaving it (use --tag to pin)"
        return 0
    fi

    local features commit
    features="$(detect_features)"
    checkout_tag "$tag"
    commit="$(git -C "$SRC" rev-parse HEAD)"
    local asset built=""
    if [ -z "$features" ] && asset="$(release_asset_name)" && [ -n "$asset" ]; then
        built="$(download_release_asset "$tag" "$asset")"
        [ -z "$built" ] || log "using the published ${asset} for ${tag}"
    fi
    if [ -z "$built" ]; then
        build_tag "$tag" "$features"
        built="${TARGET}/release/susi"
        [ "$WINDOWS" != 1 ] || built="${built}.exe"
    fi

    local got
    # SUSI_HOME pins the release root so this probe never provisions ~/.susi-dev.
    got="$(SUSI_HOME="$SUSI_HOME" "$built" --version 2>/dev/null | awk '{print $2}')"
    [ "$got" = "$want" ] || die "built binary reports version '${got}', expected '${want}'"

    wait_for_idle
    log "installing susi ${want} (was: ${have:-none}${channel:+, ${channel}})"
    swap_in "$built"
    write_marker "$want" "$features" "$commit"
    if ! restart_service; then
        rollback || true
        die "susi ${want} failed to come up; rolled back to ${have:-previous}"
    fi
    refresh_self
    log "installed susi ${want} (${features:-cpu}) from ${tag} @ ${commit:0:12}"
}

# ---- timer ------------------------------------------------------------------

install_timer() {
    command -v systemctl >/dev/null 2>&1 || die "systemd is required for the timer"
    mkdir -p "$UNIT_DIR" "$BIN_DIR"
    install -m 0755 "$(readlink -f "$0")" "$SELF_INSTALLED"
    cat >"${UNIT_DIR}/${UNIT}.service" <<EOF
[Unit]
Description=Promote the newest tagged susi release to this host
Wants=network-online.target
After=network-online.target

[Service]
Type=oneshot
ExecStart=${SELF_INSTALLED}
SuccessExitStatus=${EX_TEMPFAIL}
Nice=10
IOSchedulingClass=idle
TimeoutStartSec=3h
EOF
    cat >"${UNIT_DIR}/${UNIT}.timer" <<EOF
[Unit]
Description=Check for new susi releases hourly

[Timer]
OnBootSec=10min
OnCalendar=hourly
RandomizedDelaySec=5min
Persistent=true

[Install]
WantedBy=timers.target
EOF
    systemctl --user daemon-reload
    systemctl --user enable --now "${UNIT}.timer"
    log "timer enabled: systemctl --user list-timers ${UNIT}.timer; logs: journalctl --user -u ${UNIT}"
    log "run now:       systemctl --user start --no-block ${UNIT}.service"
}

uninstall_timer() {
    systemctl --user disable --now "${UNIT}.timer" 2>/dev/null || true
    rm -f "${UNIT_DIR}/${UNIT}.service" "${UNIT_DIR}/${UNIT}.timer" "$SELF_INSTALLED"
    systemctl --user daemon-reload 2>/dev/null || true
    log "timer removed"
}

# ---- main -------------------------------------------------------------------

main() {
    local mode=sync tag=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --check) mode=check ;;
            --tag) tag="${2:?--tag needs a value}"; shift ;;
            --rollback) mode=rollback ;;
            --install-timer) mode=install-timer ;;
            --uninstall-timer) mode=uninstall-timer ;;
            -h | --help) sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
            *) die "unknown argument: $1 (see --help)" ;;
        esac
        shift
    done

    case "$mode" in
        install-timer) install_timer; return ;;
        uninstall-timer) uninstall_timer; return ;;
        *) ;;
    esac

    mkdir -p "$SUSI_HOME"
    exec 9>"${SUSI_HOME}/release-sync.lock"
    flock -n 9 || { log "another release sync is running"; exit 0; }

    case "$mode" in
        rollback) wait_for_idle; rollback ;;
        check)
            local latest
            latest="$(latest_tag)" || die "cannot reach ${REMOTE}"
            printf 'installed: %s (%s)\nlatest:    %s\n' \
                "$(installed_version || true)" "$(installed_channel || true)" "${latest:-none}"
            ;;
        sync)
            local explicit=1
            if [ -z "$tag" ]; then
                explicit=0
                tag="$(latest_tag)" || die "cannot reach ${REMOTE}"
                [ -n "$tag" ] || die "no vX.Y.Z tags on ${REMOTE}"
            fi
            [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "not a release tag: ${tag}"
            sync_release "$tag" "$explicit"
            ;;
        *) die "unreachable mode: $mode" ;;
    esac
}

main "$@"
