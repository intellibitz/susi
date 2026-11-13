#!/usr/bin/env bash
# Promote the newest tagged susi release to this host.
#
# The local susi (~/.susi/bin/susi and the daemon it runs) is a release-only
# toolchain: it builds future susi, so dev builds never touch it (`cargo xb`
# builds without installing). This script is the one path that updates it:
#
#   1. resolve the newest vX.Y.Z tag on the release remote (git ls-remote)
#   2. read the tag's release record releases/<tag>.json from the remote's
#      default branch and check that the tag points at the commit it records
#      (docs/releases.md)
#   3. build the tag natively for this host in a private checkout
#      (~/.susi/release-src) with a private target dir — never the dev tree —
#      using the profile, features and environment the record names and the
#      compiler the tag pins in rust-toolchain.toml
#   4. verify the binary reports the tag's version (the gate), and report
#      whether its sha256 matches the release's reference build (information
#      only: an independent build may differ in bytes)
#   5. wait until no susi client is mid-mission, then swap it in atomically,
#      keeping the previous binary as susi.prev
#   6. restart the daemon on the new binary; roll back if it fails health
#
# Nothing but git objects is fetched, and nothing is asked of a git host
# beyond git itself (CEO mandate 2026-11-09: GitHub is storage only; CEO
# decision 00:22: release binaries are built from git tags).
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
#   SUSI_RELEASE_REMOTE    git URL of the release repo (default: the origin of the
#                          private release checkout, which install.sh or an
#                          earlier sync cloned; there is no built-in host)
#   SUSI_RELEASE_SRC       private release checkout (default: ~/.susi/release-src)
#   SUSI_RELEASE_TARGET    private cargo target dir (default: ~/.susi/build-cache/release-sync)
#   SUSI_RELEASE_FEATURES  cargo features override (default: the release record's
#                          features; CUDA/MKL retired). An override is not the
#                          reference build.
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
SRC="${SUSI_RELEASE_SRC:-${SUSI_HOME}/release-src}"
# The release remote is configuration: SUSI_RELEASE_REMOTE, else the origin
# the private release checkout was cloned from. No git host is built in.
REMOTE="${SUSI_RELEASE_REMOTE:-}"
if [ -z "$REMOTE" ] && [ -d "$SRC/.git" ]; then
    REMOTE="$(git -C "$SRC" remote get-url origin 2>/dev/null || true)"
fi
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
export SUSI_UNATTENDED="${SUSI_UNATTENDED:-1}"
export GIT_TERMINAL_PROMPT="${GIT_TERMINAL_PROMPT:-0}"
export GCM_INTERACTIVE="${GCM_INTERACTIVE:-never}"
export SSH_ASKPASS_REQUIRE="${SSH_ASKPASS_REQUIRE:-never}"

log() { printf '[release-sync] %s\n' "$*" >&2; }
die() { log "error: $*"; exit 1; }
need_remote() {
    [ -n "$REMOTE" ] || die "no release remote: set SUSI_RELEASE_REMOTE to the git URL of the Susi core repository (or install with install.sh, which records it in ${SRC})"
}

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

# The cargo features to build: SUSI_RELEASE_FEATURES when set (not the
# reference build), else the release record's features ($1).
detect_features() {
    local feature requested="${SUSI_BUILD_FEATURES:-} ${SUSI_RELEASE_FEATURES:-}"
    for feature in ${requested//,/ }; do
        case "$feature" in
            cuda|mkl|*/cuda|*/mkl) die "CUDA/MKL features are retired: their dependencies fail product license/native-code gates"; return 1 ;;
        esac
    done
    if [ -n "${SUSI_RELEASE_FEATURES+x}" ]; then
        printf '%s' "$SUSI_RELEASE_FEATURES"
        return
    fi
    printf '%s' "${1:-}"
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

# ---- release record ----------------------------------------------------------

RECORD=""
# The default-branch commit the record was read from; the upgrade script and
# its Windows helper are installed from there too (refresh_self).
RECORD_REF=""

# Fetch the release record of $1 from the remote's default branch into
# $RECORD and check that the tag points at the commit it records.
read_record() {
    local tag="$1" commit
    RECORD="${TARGET}/records/${tag}.json"
    mkdir -p "${TARGET}/records"
    git -C "$SRC" fetch --quiet origin HEAD
    RECORD_REF="$(git -C "$SRC" rev-parse FETCH_HEAD)"
    git -C "$SRC" show "FETCH_HEAD:releases/${tag}.json" >"${RECORD}.new" 2>/dev/null ||
        die "${tag} has no release record releases/${tag}.json on ${REMOTE}; it is not a release (docs/releases.md)"
    mv -f "${RECORD}.new" "$RECORD"
    commit="$(git -C "$SRC" rev-parse "refs/tags/${tag}^{commit}")"
    check_record "$tag" "$commit"
}

# record_field <name>: a one-line string field of $RECORD (layout pinned by
# tests/release_records.rs).
record_field() {
    sed -n "s/^[[:space:]]*\"$1\": \"\([^\"]*\)\",\{0,1\}[[:space:]]*\$/\1/p" "$RECORD" | head -n 1
}

# The release record is the authority for host support. Only a `target` inside
# `missing_targets` can make a release unavailable; asset targets and prose
# elsewhere in the record do not count.
host_target() {
    command -v rustc >/dev/null 2>&1 || die "rustc was not found; cannot determine the native host target"
    local target
    target="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p' | head -n 1 || true)"
    [ -n "$target" ] || die "rustc did not report a native host target"
    printf '%s' "$target"
}

record_missing_target() {
    local target="$1"
    awk -v wanted="$target" '
        /"missing_targets"[[:space:]]*:/ { in_missing = 1; next }
        in_missing && /"checks"[[:space:]]*:/ { exit(found ? 0 : 1) }
        in_missing && index($0, "\"target\": \"" wanted "\"") { found = 1; exit 0 }
        END { exit(found ? 0 : 1) }
    ' "$RECORD"
}

# The record names this tag and the commit it points at, and its build
# settings are plain words cargo can take.
check_record() {
    local tag="$1" commit="$2" item
    [ "$(record_field tag)" = "$tag" ] || die "releases/${tag}.json names tag '$(record_field tag)'"
    [ "$(record_field commit)" = "$commit" ] ||
        die "${tag} points at ${commit}, but releases/${tag}.json records '$(record_field commit)'; refusing to build"
    [[ "$(record_field profile)" =~ ^[A-Za-z0-9_.-]+$ ]] || die "releases/${tag}.json has no usable build profile"
    [[ "$(record_field features)" =~ ^[A-Za-z0-9_./,-]*$ ]] || die "releases/${tag}.json has unusable features"
    for item in $(record_env); do
        [[ "$item" =~ ^[A-Z_][A-Z0-9_]*=[A-Za-z0-9_./,-]*$ ]] || die "releases/${tag}.json has an unusable build.env entry: ${item}"
    done
}

# The record's build environment, one NAME=value per word.
record_env() {
    sed -n 's/^[[:space:]]*"env": \[\(.*\)\],\{0,1\}[[:space:]]*$/\1/p' "$RECORD" | head -n 1 | tr -d ' "' | tr ',' ' '
}

# record_sha <asset file>: the reference build's sha256, empty when the record
# has no reference build for it.
record_sha() {
    grep -F "\"file\": \"$1\"" "$RECORD" | sed -n 's/.*"sha256": "\([0-9a-f]\{64\}\)".*/\1/p' | head -n 1 || true
}

# This host's asset name in release records (core releases x86_64 Windows and
# x86_64 Linux only, CEO decision 2026-11-03).
host_asset() {
    case "$(uname -s)/$(uname -m)" in
        MINGW*/x86_64 | MSYS*/x86_64 | CYGWIN*/x86_64) printf 'susi-windows-x86_64.exe' ;;
        Linux/x86_64) printf 'susi-linux-x86_64' ;;
        *) ;;
    esac
}

# Report how the build compares with the release's reference build. Never a
# gate: reproducibility is not claimed (docs/releases.md). stdin keeps a
# Windows path's backslashes out of sha256sum's output.
report_reference() {
    local built="$1" asset want have
    asset="$(host_asset)"
    have="$(sha256sum <"$built" | awk '{print tolower($1)}')"
    want="$([ -z "$asset" ] || record_sha "$asset")"
    if [ -z "$want" ]; then
        log "checksum ${have} (the release records no reference build for ${asset:-this host})"
    elif [ "$want" = "$have" ]; then
        log "checksum ${have} matches the reference build"
    else
        log "checksum ${have} differs from the reference build ${want}, as expected for an independent build"
    fi
}

build_tag() {
    local tag="$1" features="$2" profile
    profile="$(record_field profile)"
    local args=(build --locked --profile "$profile" -p susi --bin susi)
    [ -n "$features" ] && args+=(--features "$features")
    local env_args=()
    read -r -a env_args <<<"$(record_env)"
    local channel
    channel="$(sed -n 's/^channel *= *"\([^"]*\)".*/\1/p' "$SRC/rust-toolchain.toml" 2>/dev/null | head -n 1)"
    if [ -n "$channel" ] && ! grep -q "^${channel}[-[:space:]]" <<<"$(rustup toolchain list 2>/dev/null || true)"; then
        die "${tag} pins Rust ${channel} (rust-toolchain.toml), which rustup does not have; install it with: rustup toolchain install ${channel} --profile minimal"
    fi

    log "building ${tag} (profile ${profile}, features: ${features:-none}${env_args[*]:+, ${env_args[*]}}) in ${SRC}"
    (cd "$SRC" && env RUSTUP_AUTO_INSTALL=0 CARGO_TARGET_DIR="$TARGET" "${env_args[@]}" nice -n 10 cargo "${args[@]}")
}

# Where build_tag leaves the binary.
built_path() {
    local profile out
    profile="$(record_field profile)"
    case "$profile" in dev | test) out=debug ;; bench) out=release ;; *) out="$profile" ;; esac
    if [ "$WINDOWS" = 1 ]; then printf '%s/%s/susi.exe' "$TARGET" "$out"; else printf '%s/%s/susi' "$TARGET" "$out"; fi
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
    case "$features" in *metal*) accel='"metal"' ;; *) ;; esac
    printf '{"profile":"release","accelerator":%s,"channel":"release","version":"%s","commit":"%s","installed_at":%s}\n' \
        "$accel" "$version" "$commit" "$(date +%s)" >"${MARKER}.new"
    mv -f "${MARKER}.new" "$MARKER"
}

swap_in() {
    local built="$1"
    mkdir -p "$BIN_DIR"
    if [ "$WINDOWS" = 1 ]; then
        [ ! -f "$MARKER" ] || cp -p "$MARKER" "${MARKER}.prev"
        if ! WINDOWS_DAEMON_WAS_RUNNING="$(windows_release swap "$built")"; then
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
    # Keep the timer's copy of this script current. It comes from the default
    # branch the release record was read from, not from the tag: the record
    # and the way a release is installed live on the default branch, and an
    # older tag's copy of this script may predate them.
    [ -f "$SELF_INSTALLED" ] && [ -n "$RECORD_REF" ] || return 0
    if git -C "$SRC" show "${RECORD_REF}:scripts/susi-release-sync.sh" >"${SELF_INSTALLED}.new" 2>/dev/null; then
        chmod 0755 "${SELF_INSTALLED}.new"
        mv -f "${SELF_INSTALLED}.new" "$SELF_INSTALLED"
    else
        rm -f "${SELF_INSTALLED}.new"
    fi
    if [ "$WINDOWS" = 1 ] &&
        git -C "$SRC" show "${RECORD_REF}:scripts/susi-release-sync-windows.ps1" >"${BIN_DIR}/susi-release-sync-windows.ps1.new" 2>/dev/null; then
        mv -f "${BIN_DIR}/susi-release-sync-windows.ps1.new" "${BIN_DIR}/susi-release-sync-windows.ps1"
    else
        rm -f "${BIN_DIR}/susi-release-sync-windows.ps1.new"
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

    local features commit built target
    checkout_tag "$tag"
    read_record "$tag"
    target="$(host_target)"
    if record_missing_target "$target"; then
        if [ "$explicit" = 0 ]; then
            log "skipping ${tag}: release record lists native host ${target} in missing_targets"
            return 0
        fi
        die "${tag} is not qualified for native host ${target}; it is listed in missing_targets"
    fi
    features="$(detect_features "$(record_field features)")" || return 1
    commit="$(git -C "$SRC" rev-parse HEAD)"
    build_tag "$tag" "$features"
    built="$(built_path)"
    [ -f "$built" ] || die "the build left no ${built}"

    local got
    # SUSI_HOME pins the release root so this probe never provisions ~/.susi-dev.
    got="$(SUSI_HOME="$SUSI_HOME" "$built" --version 2>/dev/null | awk '{print $2}')"
    [ "$got" = "$want" ] || die "built binary reports version '${got}', expected '${want}'"
    report_reference "$built"

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
Environment=SUSI_UNATTENDED=1
Environment=GIT_TERMINAL_PROMPT=0
Environment=GCM_INTERACTIVE=never
Environment=SSH_ASKPASS_REQUIRE=never
EnvironmentFile=-%h/.susi/service.env
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
            need_remote
            latest="$(latest_tag)" || die "cannot reach ${REMOTE}"
            printf 'installed: %s (%s)\nlatest:    %s\n' \
                "$(installed_version || true)" "$(installed_channel || true)" "${latest:-none}"
            ;;
        sync)
            local explicit=1
            need_remote
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
