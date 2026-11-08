#!/usr/bin/env bash

SUSI_REQUESTED_FEATURES="${SUSI_BUILD_FEATURES:-}"
for SUSI_REQUESTED_FEATURE in ${SUSI_REQUESTED_FEATURES//,/ }; do
    case "$SUSI_REQUESTED_FEATURE" in
        cuda|mkl|*/cuda|*/mkl)
            echo "SUSI_BUILD_FEATURES requests retired CUDA/MKL support; product license/native-code gates prohibit this backend." >&2
            exit 1 ;;
    esac
done
# susi installer: builds the newest release tag from source on this host.
#
# A release is a git tag plus its committed record releases/<tag>.json
# (docs/releases.md). This script resolves the tag with `git ls-remote --tags`
# against SUSI_SOURCE_REPO (or the origin of the checkout it runs from), checks
# it out, builds it with the recorded settings and the toolchain pinned in
# rust-toolchain.toml, and installs it only when it reports the tag's version.
# It downloads no binary and installs no system software.
#
# This script uses bash-only features (arrays, [[ ]], BASH_SOURCE) throughout.
# `sh` is dash/ash on most Linux distros (Debian, Ubuntu, Alpine, ...), not
# bash, so `sh install.sh` fails partway with a cryptic "Bad substitution"
# rather than running. Re-exec under bash when $0 is a real path; when the
# script arrives on stdin, point the user at bash.
if [ -z "$BASH_VERSION" ]; then
    if ! command -v bash >/dev/null 2>&1; then
        echo "Error: this installer requires bash, which was not found on PATH. Install bash and re-run." >&2
        exit 1
    fi
    if [ -f "$0" ]; then
        exec bash "$0" "$@"
    fi
    echo "Error: this installer needs to be run with bash, not sh. Re-run it as:" >&2
    echo "  bash install.sh" >&2
    exit 1
fi

set -e
set -o pipefail

# Release scope (CEO decision 2026-11-03): the free public core release is
# published for x86_64 Windows and x86_64 Linux only. macOS and non-x86_64
# Linux support stays in the source, but core publishes no binary for them;
# certified builds for other platforms come from the Susi commercial suite.
# Refuse before any directory is created, download attempted or build run.
case "$(uname -s)" in
    Darwin*)
        echo "Error: this is macOS. The free Susi core release supports x86_64 Windows and x86_64 Linux only." >&2
        echo "Other platforms, including macOS, are served by the Susi commercial suite. Nothing was installed." >&2
        exit 1
        ;;
    Linux*)
        case "$(uname -m)" in
            x86_64|amd64) ;;
            *)
                echo "Error: this is $(uname -m) Linux. The free Susi core release supports x86_64 Windows and x86_64 Linux only." >&2
                echo "Other platforms, including non-x86_64 Linux, are served by the Susi commercial suite. Nothing was installed." >&2
                exit 1
                ;;
        esac
        ;;
esac

ISOLATED_INSTALL=0
if [ -n "${SUSI_HOME:-}" ]; then
    case "$SUSI_HOME" in
        /*) ;;
        *) echo "Error: SUSI_HOME must be an absolute installation path." >&2; exit 1 ;;
    esac
    ISOLATED_INSTALL=1
fi
GLOBAL_SUSI_DIR="${SUSI_HOME:-$HOME/.susi}"
GLOBAL_BIN_DIR="$GLOBAL_SUSI_DIR/bin"

# 1. Detect Environment
OS_TYPE="$(uname -s | tr '[:upper:]' '[:lower:]')"
case "$OS_TYPE" in
    linux*)  PLATFORM="linux" ;;
    mingw*|cygwin*|msys*)
        echo "Error: Native Windows shell detected ($OS_TYPE). Use the native Windows PowerShell installer from a checkout instead:" >&2
        echo "  powershell -NoProfile -ExecutionPolicy Bypass -File ./install.ps1" >&2
        exit 1
        ;;
    *)
        echo "Error: unsupported platform $(uname -s). The free Susi core release supports x86_64 Windows and x86_64 Linux only." >&2
        exit 1
        ;;
esac

# 2. Requirements. A release is a git tag plus its committed record
# (releases/<tag>.json, docs/releases.md): this installer builds that tag from
# source on this host and installs nothing else. No binary is downloaded and no
# system software is installed; a missing tool stops the install here, before
# anything is written.
for tool in git cargo rustup; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        case "$tool" in
            git) echo "Error: git is required to fetch the Susi release tag. Install git and re-run." >&2 ;;
            *) echo "Error: $tool was not found. Susi installs by building its release tag from source, which needs a Rust toolchain managed by rustup (https://rustup.rs). Install rustup, then re-run; the release pins its compiler in rust-toolchain.toml." >&2 ;;
        esac
        exit 1
    fi
done
SHA_TOOL=""
for tool in sha256sum shasum; do
    if command -v "$tool" >/dev/null 2>&1; then SHA_TOOL="$tool"; break; fi
done
if [ -z "$SHA_TOOL" ]; then
    echo "Error: sha256sum or shasum is required to compare the build with the release's reference checksum." >&2
    exit 1
fi

# The source repository: SUSI_SOURCE_REPO, else the origin of the checkout this
# script runs from. There is deliberately no built-in default: a host name in
# this script would tie every install to one git host, and an ambient `.git` in
# the caller's working directory must never redirect the build.
SOURCE_REPO="${SUSI_SOURCE_REPO:-}"
if [ -z "$SOURCE_REPO" ] && [[ -n "${BASH_SOURCE[0]:-}" && -f "${BASH_SOURCE[0]}" ]]; then
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    if [ -f "$SCRIPT_DIR/Cargo.toml" ] && grep -q '^name *= *"susi"' "$SCRIPT_DIR/Cargo.toml"; then
        SOURCE_REPO="$(git -C "$SCRIPT_DIR" remote get-url origin 2>/dev/null || true)"
    fi
fi
if [ -z "$SOURCE_REPO" ]; then
    echo "Error: no source repository. Set SUSI_SOURCE_REPO to the git URL of the Susi core repository, or run install.sh from a clone of it." >&2
    exit 1
fi

mkdir -p "$GLOBAL_BIN_DIR"
mkdir -p "$GLOBAL_SUSI_DIR/models"
echo "Initializing susi environment (source: $SOURCE_REPO)..."

# 3. Resolve the release: the newest vX.Y.Z tag, or SUSI_RELEASE_TAG.
TAG="${SUSI_RELEASE_TAG:-}"
if [ -z "$TAG" ]; then
    TAG="$(git ls-remote --tags --refs "$SOURCE_REPO" 'v*' \
        | awk '{sub("refs/tags/", "", $2); print $2}' \
        | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' \
        | sort -V | tail -n 1 || true)"
fi
if ! [[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Error: no release tag vX.Y.Z found on $SOURCE_REPO (got '${TAG}')." >&2
    exit 1
fi
VERSION="${TAG#v}"

# 4. Check the tag out into the private release checkout the upgrade script
# (scripts/susi-release-sync.sh) also uses, and read its record from the
# repository's default branch.
SRC="$GLOBAL_SUSI_DIR/release-src"
if [ ! -d "$SRC/.git" ]; then
    echo "Cloning $SOURCE_REPO into $SRC..."
    git clone --quiet --no-checkout "$SOURCE_REPO" "$SRC"
fi
git -C "$SRC" remote set-url origin "$SOURCE_REPO"
git -C "$SRC" fetch --quiet --force origin "refs/tags/${TAG}:refs/tags/${TAG}"
git -C "$SRC" fetch --quiet origin HEAD
RECORD="$GLOBAL_SUSI_DIR/release-$TAG.json"
if ! git -C "$SRC" show "FETCH_HEAD:releases/${TAG}.json" >"$RECORD" 2>/dev/null; then
    rm -f "$RECORD"
    echo "Error: $TAG has no release record releases/${TAG}.json on $SOURCE_REPO; it is not a release (docs/releases.md)." >&2
    exit 1
fi
# record_field <name>: a one-line string field of the record (the layout is
# pinned by tests/release_records.rs).
record_field() {
    sed -n "s/^[[:space:]]*\"$1\": \"\([^\"]*\)\",\{0,1\}[[:space:]]*\$/\1/p" "$RECORD" | head -n 1
}
COMMIT="$(git -C "$SRC" rev-parse "refs/tags/${TAG}^{commit}")"
if [ "$(record_field commit)" != "$COMMIT" ] || [ "$(record_field tag)" != "$TAG" ]; then
    echo "Error: $TAG points at $COMMIT, but releases/${TAG}.json records '$(record_field commit)'. Refusing to build." >&2
    exit 1
fi
git -C "$SRC" -c advice.detachedHead=false checkout --quiet --force --detach "refs/tags/${TAG}"
git -C "$SRC" clean --quiet -ffdx

# 5. The pinned compiler. rustup must already have it: this installer never
# installs a toolchain on its own.
CHANNEL="$(sed -n 's/^channel *= *"\([^"]*\)".*/\1/p' "$SRC/rust-toolchain.toml" 2>/dev/null | head -n 1)"
TOOLCHAINS="$(rustup toolchain list 2>/dev/null || true)"
if [ -n "$CHANNEL" ] && ! grep -q "^${CHANNEL}[-[:space:]]" <<<"$TOOLCHAINS"; then
    echo "Error: $TAG pins Rust $CHANNEL (rust-toolchain.toml), which rustup does not have. Install it, then re-run:" >&2
    echo "  rustup toolchain install $CHANNEL --profile minimal" >&2
    exit 1
fi

# 6. Build with the recorded profile, features and environment.
PROFILE="$(record_field profile)"
FEATURES="$(record_field features)"
ENV_LINE="$(sed -n 's/^[[:space:]]*"env": \[\(.*\)\],\{0,1\}[[:space:]]*$/\1/p' "$RECORD" | head -n 1)"
SAFE='^[A-Za-z0-9_./,-]*$'
if [ -z "$PROFILE" ] || ! [[ "$PROFILE" =~ $SAFE ]] || ! [[ "$FEATURES" =~ $SAFE ]]; then
    echo "Error: releases/${TAG}.json has no usable build profile or features." >&2
    exit 1
fi
BUILD_ENV=()
IFS=',' read -r -a ENV_ITEMS <<<"$ENV_LINE"
for item in "${ENV_ITEMS[@]}"; do
    item="$(printf '%s' "$item" | tr -d ' "')"
    [ -n "$item" ] || continue
    if ! [[ "$item" =~ ^[A-Z_][A-Z0-9_]*=[A-Za-z0-9_./,-]*$ ]]; then
        echo "Error: releases/${TAG}.json has an unusable build.env entry: $item" >&2
        exit 1
    fi
    BUILD_ENV+=("$item")
done
CARGO_ARGS=(build --locked --profile "$PROFILE" -p susi --bin susi)
[ -z "$FEATURES" ] || CARGO_ARGS+=(--features "$FEATURES")

# Reuse a compiler cache the user already has; never install one.
if command -v sccache >/dev/null 2>&1 && [ -z "${RUSTC_WRAPPER:-}" ]; then
    export RUSTC_WRAPPER=sccache
    echo "  Using sccache for faster recompiles."
fi
BUILD_TARGET_DIR="${CARGO_TARGET_DIR:-$GLOBAL_SUSI_DIR/build-cache/release-sync}"
mkdir -p "$BUILD_TARGET_DIR"

# Heartbeat so a long silent stretch (one large crate can take minutes)
# does not read as a frozen script.
HEARTBEAT_PID=""
stop_heartbeat() {
    if [ -n "$HEARTBEAT_PID" ]; then
        kill "$HEARTBEAT_PID" 2>/dev/null || true
        wait "$HEARTBEAT_PID" 2>/dev/null || true
        HEARTBEAT_PID=""
    fi
}
trap stop_heartbeat EXIT
# The subshell stops its own sleep when it is stopped, so no orphan keeps the
# installer's output open after the build.
( SECS=0
  NAP=""
  trap '[ -z "$NAP" ] || kill "$NAP" 2>/dev/null; exit 0' TERM
  while true; do
      sleep 30 &
      NAP=$!
      wait "$NAP"
      SECS=$((SECS+30))
      echo "  ...still building (${SECS}s elapsed, this is normal for a cold build)"
  done
) &
HEARTBEAT_PID=$!

echo "Building susi $TAG ($COMMIT) with Rust ${CHANNEL:-as pinned}: cargo ${CARGO_ARGS[*]} ${BUILD_ENV[*]}"
echo "A cold build takes several minutes."
(cd "$SRC" && env RUSTUP_AUTO_INSTALL=0 CARGO_TARGET_DIR="$BUILD_TARGET_DIR" "${BUILD_ENV[@]}" cargo "${CARGO_ARGS[@]}")
stop_heartbeat

case "$PROFILE" in dev | test) OUT_DIR=debug ;; bench) OUT_DIR=release ;; *) OUT_DIR="$PROFILE" ;; esac
ENGINE_SRC="$BUILD_TARGET_DIR/$OUT_DIR/susi"
if [ ! -f "$ENGINE_SRC" ]; then
    echo "Error: the build produced no $ENGINE_SRC. Nothing was installed." >&2
    exit 1
fi

# 7. The gate: the built binary must report the tag's version.
REPORTED="$(SUSI_HOME="$GLOBAL_SUSI_DIR" "$ENGINE_SRC" --version 2>/dev/null | awk '{print $2}' | head -n 1)"
if [ "$REPORTED" != "$VERSION" ]; then
    echo "Error: the built binary reports version '${REPORTED}', expected '${VERSION}'. Nothing was installed." >&2
    exit 1
fi
# Informational: compare with the reference build's checksum. Most independent
# builds differ in bytes (reproducibility is not claimed), so a difference is
# reported, never enforced.
ASSET="susi-linux-x86_64"
WANT="$(grep -F "\"file\": \"$ASSET\"" "$RECORD" | sed -n 's/.*"sha256": "\([0-9a-f]\{64\}\)".*/\1/p' | head -n 1 || true)"
if [ "$SHA_TOOL" = shasum ]; then
    HAVE="$(shasum -a 256 "$ENGINE_SRC" | awk '{print $1}')"
else
    HAVE="$(sha256sum "$ENGINE_SRC" | awk '{print $1}')"
fi
if [ -z "$WANT" ]; then
    echo "  checksum: $HAVE (the release records no reference build for $ASSET)"
elif [ "$WANT" = "$HAVE" ]; then
    echo "  checksum: $HAVE matches the reference build"
else
    echo "  checksum: $HAVE differs from the reference build $WANT, as expected for an independent build (docs/releases.md)"
fi

pkill -f "$GLOBAL_BIN_DIR/susi" || true
rm -f "$GLOBAL_BIN_DIR/susi" 2>/dev/null || true
cp "$ENGINE_SRC" "$GLOBAL_BIN_DIR/susi"
chmod +x "$GLOBAL_BIN_DIR/susi"
rm -f "$RECORD"
echo "Deployed susi $VERSION to $GLOBAL_BIN_DIR"

# 4. Opening performs discovery without blocking the first goal prompt.

# 5. Daemon Persistence
# susi self-daemonizes on demand: every CLI invocation checks for a running
# daemon and spawns one detached (scoped to the invoking directory) if absent.
# No service registration is needed for that path - the daemon starts the
# first time a goal needs it. On Linux, when a systemd user manager is reachable, the
# spawn lands in a real cgroup (MemoryMax=2G, CPUQuota=50%) via systemd-run.
#
# SUSI_ALWAYS_ON=1 additionally registers a boot-persistent service
# (systemd user unit / launchd agent) that starts the daemon at login and
# restarts it on exit - opt-in because an always-on background process is a
# resource/UX commitment the user should choose, not a silent default.
# SUSI_NO_DAEMON=1 skips this section entirely (self-daemonization still
# applies at runtime regardless).
if [ "$ISOLATED_INSTALL" = "1" ]; then
    echo "Skipping service registration for isolated installation."
elif [ -n "$SUSI_NO_DAEMON" ]; then
    echo "Skipping service registration (SUSI_NO_DAEMON is set)."
elif [ -n "$SUSI_ALWAYS_ON" ] && [[ "$PLATFORM" == "linux" ]]; then
    if command -v systemctl >/dev/null 2>&1 && [ "$EUID" -ne 0 ]; then
        echo "Registering susi daemon with systemd (Always-On Mode, SUSI_ALWAYS_ON=1)."
        mkdir -p "$HOME/.config/systemd/user"
        cat <<EOF > "$HOME/.config/systemd/user/susi.service"
[Unit]
Description=susi Intelligence Substrate Daemon (Always-On)
After=network.target

[Service]
ExecStart=$GLOBAL_BIN_DIR/susi daemon-start --workspace $HOME
Restart=always
RestartSec=5
MemoryMax=2G
CPUQuota=50%

[Install]
WantedBy=default.target
EOF
        # Enable lingering so the user systemd instance (and this service)
        # keeps running after logout/reboot without an active login session -
        # best effort; some systems restrict this to root/polkit-approved users.
        if command -v loginctl >/dev/null 2>&1; then
            loginctl enable-linger "$(id -un)" 2>/dev/null || true
        fi

        # `systemctl --user` requires a reachable user session bus, which
        # isn't always up (e.g. a fresh SSH session before lingering takes
        # effect, WSL without systemd, minimal containers). Without this
        # guard a failure here would abort the whole installer under `set -e`
        # even though the binary itself installed fine.
        if systemctl --user daemon-reload 2>/dev/null \
            && systemctl --user enable susi.service 2>/dev/null \
            && systemctl --user start susi.service 2>/dev/null; then
            echo "  Always-on service registered. Manage: systemctl --user [start|stop|status] susi.service"
        else
            echo "  Warning: could not reach the systemd user session bus; skipping service start."
            echo "  susi still self-daemonizes on first use - no action needed."
        fi
    fi
elif [ -n "$SUSI_ALWAYS_ON" ] && [[ "$PLATFORM" == "macos" ]]; then
    echo "Registering susi daemon with launchd (Always-On Mode, SUSI_ALWAYS_ON=1)."
    LAUNCHD_PLIST="$HOME/Library/LaunchAgents/com.susi.daemon.plist"
    cat <<EOF > "$LAUNCHD_PLIST"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.susi.daemon</string>
    <key>ProgramArguments</key>
    <array>
        <string>$GLOBAL_BIN_DIR/susi</string>
        <string>daemon-start</string>
        <string>--workspace</string>
        <string>$HOME</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ThrottleInterval</key>
    <integer>30</integer>
</dict>
</plist>
EOF
    if ! launchctl load "$LAUNCHD_PLIST" 2>/dev/null; then
        echo "  Warning: could not register with launchd; skipping service start."
        echo "  susi still self-daemonizes on first use - no action needed."
    fi
else
    echo "No service registration needed: susi self-daemonizes on first use (SUSI_ALWAYS_ON=1 for boot persistence)."
fi

# 6. PATH Management
if [ "$ISOLATED_INSTALL" = "0" ] && [[ ":$PATH:" != *":$GLOBAL_BIN_DIR:"* ]]; then
    ADDED_PATH=0
    CONFIG_FILES=("$HOME/.bashrc" "$HOME/.zshrc" "$HOME/.profile")
    for config in "${CONFIG_FILES[@]}"; do
        if [ -f "$config" ]; then
            if ! grep -q "\.susi/bin" "$config"; then
                echo -e "\n# susi path initialization\nexport PATH=\"\$HOME/.susi/bin:\$PATH\"" >> "$config"
            fi
            ADDED_PATH=1
        fi
    done

    if [ "$ADDED_PATH" = "0" ]; then
        echo -e "\n# susi path initialization\nexport PATH=\"\$HOME/.susi/bin:\$PATH\"" >> "$HOME/.profile"
    fi

    # Export for the rest of the script/session execution
    export PATH="$GLOBAL_BIN_DIR:$PATH"

    FISH_CONFIG="$HOME/.config/fish/config.fish"
    if [ -d "$HOME/.config/fish" ] || command -v fish >/dev/null 2>&1; then
        mkdir -p "$HOME/.config/fish"
        if [ -f "$FISH_CONFIG" ] && ! grep -q "\.susi/bin" "$FISH_CONFIG"; then
            echo -e "\n# susi path initialization\nfish_add_path \$HOME/.susi/bin" >> "$FISH_CONFIG"
        elif [ ! -f "$FISH_CONFIG" ]; then
            echo -e "fish_add_path \$HOME/.susi/bin" > "$FISH_CONFIG"
        fi
        if command -v fish >/dev/null 2>&1; then
            fish -c "fish_add_path $GLOBAL_BIN_DIR" >/dev/null 2>&1 || true
        fi
    fi
fi

# 7. Finalize. Core has no interactive prompt: it takes one-shot goals, and the
# conversation lives in the Susi desktop app. Show usage once on a terminal;
# never in headless automation. `--help` never reads the installer's stdin.
export PATH="$GLOBAL_BIN_DIR:$PATH"
echo "Susi is installed. No terminal restart is needed."
if [ -t 1 ] && [ -z "${NONINTERACTIVE:-}" ] && [ -z "${SUSI_NO_DAEMON:-}" ] && [ -x "$GLOBAL_BIN_DIR/susi" ]; then
    "$GLOBAL_BIN_DIR/susi" --help
fi
printf 'Open Susi and give it a goal: %q "<what you want done>"\n' "$GLOBAL_BIN_DIR/susi"
