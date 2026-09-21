#!/usr/bin/env bash
# susi installer - Lightning Fast Intelligence Substrate Onboarding
#
# This script uses bash-only features (arrays, [[ ]], BASH_SOURCE) throughout.
# `sh` is dash/ash on most Linux distros (Debian, Ubuntu, Alpine, ...), not
# bash, so `curl ... | sh` or `sh install.sh` fails partway with a cryptic
# "Bad substitution"/syntax error rather than running. Re-exec under bash:
# - downloaded-then-run (`sh install.sh`): $0 is a real path, re-exec it.
# - piped directly (`curl ... | sh`): $0 isn't a readable path (it's e.g.
#   "sh"), and stdin has already been partially consumed by the outer shell,
#   so a reliable re-exec isn't possible here - point the user at `| bash`.
if [ -z "$BASH_VERSION" ]; then
    if ! command -v bash >/dev/null 2>&1; then
        echo "Error: this installer requires bash, which was not found on PATH. Install bash and re-run." >&2
        exit 1
    fi
    if [ -f "$0" ]; then
        exec bash "$0" "$@"
    fi
    echo "Error: this installer needs to be run with bash, not sh. Re-run as:" >&2
    echo "  curl -sSfL https://raw.githubusercontent.com/intellibitz/susi/main/install.sh | bash" >&2
    exit 1
fi

set -e
set -o pipefail

GLOBAL_SUSI_DIR="$HOME/.susi"
GLOBAL_BIN_DIR="$GLOBAL_SUSI_DIR/bin"
mkdir -p "$GLOBAL_BIN_DIR"
mkdir -p "$GLOBAL_SUSI_DIR/models"

# SUSI_REPO is intentionally only settable via env var, never auto-detected
# from an ambient `.git` remote in the current directory - this script is
# usually run via `curl ... | sh` from an arbitrary CWD, and trusting a
# stray local git remote there would let an unrelated repo silently redirect
# the binary download/build to itself.
SUSI_REPO="${SUSI_REPO:-intellibitz/susi}"

echo "Initializing susi environment (Repo: $SUSI_REPO)..."

# sha256_verify <file> <expected-hex-digest>
sha256_verify() {
    local file="$1" expected="$2" actual
    if [ -z "$expected" ]; then
        return 1
    fi
    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$file" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
        actual=$(shasum -a 256 "$file" | awk '{print $1}')
    else
        echo "  Error: no sha256sum/shasum available to verify checksum. Refusing to install unverified binary." >&2
        return 1
    fi
    [ "$actual" = "$expected" ]
}

# 1. Detect Environment
OS_TYPE="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH_TYPE="$(uname -m)"

case "$OS_TYPE" in
    linux*)  PLATFORM="linux" ;;
    darwin*) PLATFORM="macos" ;;
    mingw*|cygwin*|msys*)
        echo "Error: Native Windows is not currently supported." >&2
        echo "Please install susi via Windows Subsystem for Linux (WSL)." >&2
        exit 1
        ;;
    *)       PLATFORM="unknown" ;;
esac

case "$ARCH_TYPE" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *)      ARCH="unknown" ;;
esac

INSTALLED=0

check_is_susi_source() {
    local toml_path="$1"
    if [ -f "$toml_path" ] && grep -q '^name *= *"susi"' "$toml_path"; then
        return 0
    fi
    return 1
}

HAS_LOCAL_SOURCE=0
if check_is_susi_source "Cargo.toml"; then
    HAS_LOCAL_SOURCE=1
elif [[ -n "${BASH_SOURCE[0]}" ]] && check_is_susi_source "$(dirname "${BASH_SOURCE[0]}")/Cargo.toml"; then
    HAS_LOCAL_SOURCE=1
fi

# 2. Try Binary Download First (Lightning Fast).
# Prefer a published release binary even when running from a local checkout —
# cold source builds of this workspace are tens of minutes; the prebuilt path
# is seconds. Developers who need to install *this* tree's WIP binary opt into
# source with SUSI_FORCE_SOURCE=1.
if [ -z "${SUSI_FORCE_SOURCE:-}" ] && [[ "$PLATFORM" != "unknown" && "$ARCH" != "unknown" ]]; then
    # Download the susi binary
    GPU_SUFFIX=""
    if [[ "$PLATFORM" == "linux" ]] && command -v nvidia-smi >/dev/null 2>&1; then
        if nvidia-smi --query-gpu=name --format=csv,noheader >/dev/null 2>&1; then
            GPU_SUFFIX="-cuda"
            echo "GPU Environment detected (nvidia-smi). Will request GPU-accelerated binary."
        fi
    fi

    ENGINE_BINARY="susi-$PLATFORM-$ARCH$GPU_SUFFIX"
    BASE_URL="https://github.com/$SUSI_REPO/releases/latest/download"

    # The CUDA build bundles its own cuBLAS/cuDART shared libraries next to
    # the binary (their SONAME is tied to the exact CUDA major version the
    # release was built with - a machine with a *different* major version
    # installed, even a newer one, won't have that exact file and the
    # binary fails to even start) - published as a tarball containing the
    # binary plus its lib/ dir, instead of a raw executable. Every other
    # asset is still a raw binary + .sha256.
    ASSET_IS_ARCHIVE=0
    DOWNLOAD_NAME="$ENGINE_BINARY"
    if [ "$GPU_SUFFIX" = "-cuda" ]; then
        ASSET_IS_ARCHIVE=1
        DOWNLOAD_NAME="$ENGINE_BINARY.tar.gz"
    fi

    if [ "$HAS_LOCAL_SOURCE" = "1" ]; then
        echo "Local source detected; still preferring prebuilt binary (set SUSI_FORCE_SOURCE=1 to build this checkout)."
    fi
    echo "Attempting to download pre-compiled engine binary from $SUSI_REPO..."

    DEPLOYED=0
    ENGINE_TMP="$GLOBAL_BIN_DIR/susi-new"
    DOWNLOAD_TMP="$GLOBAL_BIN_DIR/$DOWNLOAD_NAME"
    CHECKSUM_TMP="$DOWNLOAD_TMP.sha256"
    # The binary and its .sha256 are two independent downloads (each pays its
    # own DNS/TLS/redirect-to-release-asset round trip) - fetch them
    # concurrently instead of one after another. DEPLOYED is still only set
    # when both succeed, identical to the previous sequential `&&` chain.
    if command -v curl >/dev/null 2>&1; then
        echo "  Downloading engine: $DOWNLOAD_NAME..."
        curl -sSfL --connect-timeout 15 --speed-limit 1024 --speed-time 30 "$BASE_URL/$DOWNLOAD_NAME" -o "$DOWNLOAD_TMP" &
        BIN_DL_PID=$!
        curl -sSfL --connect-timeout 15 "$BASE_URL/$DOWNLOAD_NAME.sha256" -o "$CHECKSUM_TMP" &
        SHA_DL_PID=$!
        BIN_DL_OK=0
        SHA_DL_OK=0
        if wait "$BIN_DL_PID"; then BIN_DL_OK=1; fi
        if wait "$SHA_DL_PID"; then SHA_DL_OK=1; fi
        if [ "$BIN_DL_OK" = "1" ] && [ "$SHA_DL_OK" = "1" ]; then
            DEPLOYED=1
        fi
    elif command -v wget >/dev/null 2>&1; then
        echo "  Downloading engine: $DOWNLOAD_NAME..."
        wget -q --timeout=30 --tries=2 "$BASE_URL/$DOWNLOAD_NAME" -O "$DOWNLOAD_TMP" &
        BIN_DL_PID=$!
        wget -q --timeout=30 --tries=2 "$BASE_URL/$DOWNLOAD_NAME.sha256" -O "$CHECKSUM_TMP" &
        SHA_DL_PID=$!
        BIN_DL_OK=0
        SHA_DL_OK=0
        if wait "$BIN_DL_PID"; then BIN_DL_OK=1; fi
        if wait "$SHA_DL_PID"; then SHA_DL_OK=1; fi
        if [ "$BIN_DL_OK" = "1" ] && [ "$SHA_DL_OK" = "1" ]; then
            DEPLOYED=1
        fi
    fi

    # Never run a downloaded binary without verifying it against the
    # published checksum first - a missing/mismatched checksum falls back
    # to a source build rather than executing an unverified binary.
    if [ "$DEPLOYED" = "1" ]; then
        EXPECTED_SHA=$(awk '{print $1}' "$CHECKSUM_TMP" 2>/dev/null || true)
        if ! sha256_verify "$DOWNLOAD_TMP" "$EXPECTED_SHA"; then
            echo "  Checksum verification failed for $DOWNLOAD_NAME; discarding download and falling back to source build."
            DEPLOYED=0
        fi
        rm -f "$CHECKSUM_TMP"
    fi

    EXTRACTED_LIB_DIR=""
    EXTRACT_DIR=""
    if [ "$DEPLOYED" = "1" ] && [ "$ASSET_IS_ARCHIVE" = "1" ]; then
        EXTRACT_DIR=$(mktemp -d)
        if tar -xzf "$DOWNLOAD_TMP" -C "$EXTRACT_DIR" && [ -f "$EXTRACT_DIR/$ENGINE_BINARY" ]; then
            mv "$EXTRACT_DIR/$ENGINE_BINARY" "$ENGINE_TMP"
            [ -d "$EXTRACT_DIR/lib" ] && EXTRACTED_LIB_DIR="$EXTRACT_DIR/lib"
        else
            echo "  Archive extraction failed for $DOWNLOAD_NAME; discarding and falling back to build."
            DEPLOYED=0
        fi
        rm -f "$DOWNLOAD_TMP"
    elif [ "$DEPLOYED" = "1" ]; then
        mv "$DOWNLOAD_TMP" "$ENGINE_TMP"
    fi

    # Check executable before swapping
    if [ "$DEPLOYED" = "1" ]; then
        chmod +x "$ENGINE_TMP"
        if ! "$ENGINE_TMP" --version >/dev/null 2>&1 && ! "$ENGINE_TMP" --help >/dev/null 2>&1; then
            echo "  Downloaded executable validation failed; discarding and falling back to build."
            DEPLOYED=0
        fi
    fi

    if [ "$DEPLOYED" = "1" ]; then
        # Match the process cmdline so a running daemon is caught.
        pkill -f "$GLOBAL_BIN_DIR/susi" || true

        # Transactional deployment
        rm -f "$GLOBAL_BIN_DIR/susi" "$GLOBAL_BIN_DIR/susi" 2>/dev/null || true
        rm -rf "$GLOBAL_BIN_DIR/lib" 2>/dev/null || true
        mv "$ENGINE_TMP" "$GLOBAL_BIN_DIR/susi"
        if [ -n "$EXTRACTED_LIB_DIR" ]; then
            mv "$EXTRACTED_LIB_DIR" "$GLOBAL_BIN_DIR/lib"
        fi
        INSTALLED=1
        echo "Successfully deployed the susi binary from GitHub ($SUSI_REPO)."
    else
        echo "Binary download unavailable or failed. Falling back to build."
        rm -f "$ENGINE_TMP" "$DOWNLOAD_TMP" 2>/dev/null || true
    fi
    [ -n "$EXTRACT_DIR" ] && rm -rf "$EXTRACT_DIR" 2>/dev/null || true
elif [ -n "${SUSI_FORCE_SOURCE:-}" ]; then
    echo "SUSI_FORCE_SOURCE=1: skipping prebuilt download, building from source."
fi

# 3. Fallback to Local Source or Clone & Build
if [ "$INSTALLED" = "0" ]; then
    echo "Binary download unavailable or failed. Falling back to build from source..."

    # Detect if we are running from a local file or piped
    SCRIPT_DIR_DETECT=""
    if [[ -n "${BASH_SOURCE[0]}" && -f "${BASH_SOURCE[0]}" ]]; then
        SCRIPT_DIR_DETECT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd 2>/dev/null || echo "")"
    fi

    # SOURCE_IS_TEMP is true when we extracted a GitHub source archive into a
    # throwaway mktemp dir (vs a real local checkout). Kept for messaging;
    # the persistent build-cache redirect below applies to both paths unless
    # the user opts out.
    SOURCE_IS_TEMP=0
    if [[ -n "$SCRIPT_DIR_DETECT" && -f "$SCRIPT_DIR_DETECT/Cargo.toml" ]]; then
        SCRIPT_DIR="$SCRIPT_DIR_DETECT"
        echo "Using local source directory: $SCRIPT_DIR"
    else
        echo "Downloading susi source archive ($SUSI_REPO)..."
        TEMP_DIR=$(mktemp -d)
        SOURCE_URL="https://github.com/$SUSI_REPO/archive/refs/heads/main.tar.gz"

        # Move to temp dir to avoid CWD errors if the user is in a deleted directory
        cd "$TEMP_DIR" || { echo "Failed to enter temporary directory."; exit 1; }

        if command -v curl >/dev/null 2>&1 && command -v tar >/dev/null 2>&1; then
            curl -sSfL --connect-timeout 15 --speed-limit 1024 --speed-time 30 "$SOURCE_URL" | tar -xzC "$TEMP_DIR" --strip-components=1 || { echo "Source download failed."; exit 1; }
            SCRIPT_DIR="$TEMP_DIR"
        elif command -v wget >/dev/null 2>&1 && command -v tar >/dev/null 2>&1; then
            wget -qO- --timeout=30 --tries=2 "$SOURCE_URL" | tar -xzC "$TEMP_DIR" --strip-components=1 || { echo "Source download failed."; exit 1; }
            SCRIPT_DIR="$TEMP_DIR"
        else
            echo "Error: 'tar' and either 'curl' or 'wget' are required for source fallback."
            exit 1
        fi
        SOURCE_IS_TEMP=1
    fi

    if command -v cargo >/dev/null 2>&1 && [ -f "$SCRIPT_DIR/Cargo.toml" ]; then
        echo "Building release binaries from source. This crate has a large dependency"
        echo "tree (candle, wasmer, tantivy, ...) — a cold build commonly takes several"
        echo "minutes. A heartbeat below confirms progress during long silent stretches."

        # 100% GPU Hardware Interrogation Build Strategy
        BUILD_FEATURES=""
        if [[ "$PLATFORM" == "macos" ]]; then
            BUILD_FEATURES="--features metal"
        elif command -v nvcc >/dev/null 2>&1 || [ -d "/usr/local/cuda" ]; then
            CUDA_VERSION=$(nvcc --version 2>/dev/null | grep "release" | sed 's/.*release //;s/,.*//' || echo "0")
            if [[ "$CUDA_VERSION" == "11."* ]] || [[ "$CUDA_VERSION" == "12."* ]] || [[ "$CUDA_VERSION" == "13."* ]]; then
                BUILD_FEATURES="--features cuda"
                # cudarc (candle's CUDA backend) pins an exact allowlist of CUDA
                # toolkit versions and panics on any newer point release it
                # hasn't added yet (as of cudarc 0.19.9, that ceiling is 13.3).
                # Clamp newer CUDA 13.x releases down to 13.3 via cudarc's own
                # override env var - CUDA maintains ABI compatibility within a
                # major version, so this is safe rather than silently building
                # CPU-only with no warning, which is what happened here before.
                CUDA_MAJOR="${CUDA_VERSION%%.*}"
                CUDA_MINOR="${CUDA_VERSION#*.}"
                if [[ "$CUDA_MAJOR" == "13" ]] && [[ "$CUDA_MINOR" =~ ^[0-9]+$ ]] && [ "$CUDA_MINOR" -gt 3 ]; then
                    export CUDARC_CUDA_VERSION="13030"
                fi
            fi
        fi

        # Visibility for the common case the check above can't act on: most
        # end users running this installer have an NVIDIA GPU and its driver,
        # but not the multi-GB CUDA toolkit (nvcc) — which is genuinely
        # required to compile --features cuda, not optional, so this can't
        # just be flipped on. Without this, that host silently gets a
        # CPU-only build with zero indication a GPU went unused. nvidia-smi
        # is driver-level (no toolkit required), so it's the accurate signal
        # for "a GPU exists here" independent of whether the toolkit that
        # would let a build actually use it is installed (mirrors the same
        # check build.rs makes for a manual `cargo build`, EV-2022920-036).
        if [[ "$PLATFORM" == "linux" ]] && [ -z "$BUILD_FEATURES" ] && command -v nvidia-smi >/dev/null 2>&1; then
            if nvidia-smi --query-gpu=name --format=csv,noheader >/dev/null 2>&1; then
                echo "[GPU] NVIDIA GPU detected, but the CUDA toolkit (nvcc) was not found —"
                echo "[GPU] building CPU-only. Install the CUDA toolkit and re-run this installer"
                echo "[GPU] (or run ./build-gpu.sh from a source checkout) for GPU-accelerated inference."
            fi
        fi

        # Prefer a fast linker when available — mold/lld cut release link time
        # dramatically on the heavy final crate (susi → susi-daemon → …).
        # Do not override an explicit user RUSTFLAGS; append only.
        #
        # If mold is missing on Linux, bootstrap a user-local *prefix* into
        # ~/.susi/mold (bin/ld.mold + lib/mold-wrapper.so). A lone mold binary
        # is not enough for -fuse-ld / mold -run. Opt out: SUSI_SKIP_MOLD_BOOTSTRAP=1.
        ensure_mold() {
            if command -v ld.mold >/dev/null 2>&1 || [ -x "${HOME}/.susi/mold/bin/ld.mold" ]; then
                export PATH="${HOME}/.susi/mold/bin:${PATH}"
                return 0
            fi
            if [[ "$PLATFORM" != "linux" ]] || [ -n "${SUSI_SKIP_MOLD_BOOTSTRAP:-}" ]; then
                return 0
            fi
            case "$ARCH" in
                x86_64|aarch64) ;;
                *) return 0 ;;
            esac
            # Pin for reproducible installs; override with SUSI_MOLD_VERSION.
            local ver="${SUSI_MOLD_VERSION:-2.40.4}"
            local name="mold-${ver}-${ARCH}-linux"
            local url="https://github.com/rui314/mold/releases/download/v${ver}/${name}.tar.gz"
            local prefix="$GLOBAL_SUSI_DIR/mold"
            local tmp
            echo "  Bootstrapping mold ${ver} into $prefix (faster release links)..."
            tmp=$(mktemp -d)
            mkdir -p "$prefix"
            if command -v curl >/dev/null 2>&1 \
                && curl -sSfL --connect-timeout 15 --speed-limit 1024 --speed-time 30 "$url" \
                    | tar -xz -C "$tmp" \
                && [ -x "$tmp/$name/bin/ld.mold" ]; then
                # Replace prefix contents atomically-ish.
                rm -rf "${prefix}.new"
                mv "$tmp/$name" "${prefix}.new"
                rm -rf "$prefix"
                mv "${prefix}.new" "$prefix"
                export PATH="$prefix/bin:$PATH"
                # Drop a stale lone-binary install from older installer versions.
                if [ -f "$GLOBAL_BIN_DIR/mold" ] && [ ! -d "$GLOBAL_BIN_DIR/mold" ]; then
                    rm -f "$GLOBAL_BIN_DIR/mold"
                fi
                echo "  Installed mold -> $prefix/bin/mold"
            else
                echo "  Warning: could not bootstrap mold; continuing with system linker."
            fi
            rm -rf "$tmp"
        }
        ensure_mold

        # Rely on .cargo/fast-linker (clang + ld.mold / lld) from the source
        # tree when present. Avoid `mold -run` here: it wraps gcc and breaks on
        # some distros (GCC 16 libgcc_s_asneeded). Fall back to RUSTFLAGS+lld
        # only when the tree has no fast-linker wrapper.
        if [[ "$PLATFORM" == "linux" ]]; then
            if [ -x "$SCRIPT_DIR/.cargo/fast-linker" ]; then
                echo "  Using .cargo/fast-linker (mold/lld when available)."
            elif command -v clang >/dev/null 2>&1 && [ -x "${HOME}/.susi/mold/bin/ld.mold" ]; then
                export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C linker=clang -C link-arg=-fuse-ld=${HOME}/.susi/mold/bin/ld.mold"
                echo "  Using clang + mold (ld.mold) for faster linking."
            elif command -v clang >/dev/null 2>&1 && command -v ld.lld >/dev/null 2>&1; then
                export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C link-arg=-fuse-ld=lld"
                echo "  Using lld linker for faster linking."
            fi
        fi

        # Reuse compiler cache across installs when the user already has one.
        # (We do not cargo-install sccache here — that would rival a cold build.)
        if command -v sccache >/dev/null 2>&1 && [ -z "${RUSTC_WRAPPER:-}" ]; then
            export RUSTC_WRAPPER=sccache
            echo "  Using sccache for faster recompiles."
        elif [ -z "${RUSTC_WRAPPER:-}" ]; then
            echo "  Tip: install sccache (https://github.com/mozilla/sccache) to speed repeat source builds."
        fi

        # wasm32-wasip1 is only needed at runtime for neural reflex synthesis,
        # not to compile the susi binary — fetch it in parallel with the build.
        WASM_PID=""
        if command -v rustup >/dev/null 2>&1 || [ -f "$HOME/.cargo/bin/rustup" ]; then
            echo "Installing wasm32-wasip1 toolchain for Neural Reflex Generation (parallel with build)..."
            (
                if command -v rustup >/dev/null 2>&1; then
                    rustup target add wasm32-wasip1
                else
                    "$HOME/.cargo/bin/rustup" target add wasm32-wasip1
                fi
            ) >/dev/null 2>&1 &
            WASM_PID=$!
        fi

        # Heartbeat so a long silent stretch (cargo prints a new line only when a
        # compilation unit starts/finishes, and a single large crate can take
        # several minutes) doesn't read as a frozen script.
        HEARTBEAT_PID=""
        start_heartbeat() {
            ( SECS=0
              while true; do
                  sleep 30
                  SECS=$((SECS+30))
                  echo "  ...still building (${SECS}s elapsed, this is normal for a cold build)"
              done
            ) &
            HEARTBEAT_PID=$!
            disown "$HEARTBEAT_PID" 2>/dev/null || true
        }
        stop_heartbeat() {
            if [ -n "$HEARTBEAT_PID" ]; then
                kill "$HEARTBEAT_PID" 2>/dev/null || true
                wait "$HEARTBEAT_PID" 2>/dev/null || true
                HEARTBEAT_PID=""
            fi
        }
        wait_wasm() {
            if [ -n "$WASM_PID" ]; then
                wait "$WASM_PID" 2>/dev/null || true
                WASM_PID=""
            fi
        }
        trap 'stop_heartbeat; wait_wasm' EXIT

        # Build only the installable binary (-p susi), not workspace extras
        # (xtask, …). --locked skips registry re-resolution when Cargo.lock is
        # present (always true for the GitHub source archive / local checkout).
        echo "  Building engine..."
        start_heartbeat
        LOCKED_FLAG=""
        [ -f "$SCRIPT_DIR/Cargo.lock" ] && LOCKED_FLAG="--locked"

        # Where cargo will place the binary. Track it so ENGINE_SRC matches
        # what cargo actually used (including an ambient CARGO_TARGET_DIR).
        BUILD_TARGET_DIR="${CARGO_TARGET_DIR:-$SCRIPT_DIR/target}"

        # Persistent build cache under ~/.susi/build-cache so every install.sh
        # source fallback (temp extract *and* local checkout) reuses candle/
        # wasmer/… artifacts across runs. Opt out with SUSI_USE_REPO_TARGET=1
        # (keep in-repo target/ for IDE / cargo test workflows) or by setting
        # CARGO_TARGET_DIR yourself.
        if [ -z "${CARGO_TARGET_DIR:-}" ] && [ -z "${SUSI_USE_REPO_TARGET:-}" ]; then
            BUILD_TARGET_DIR="$GLOBAL_SUSI_DIR/build-cache"
            export CARGO_TARGET_DIR="$BUILD_TARGET_DIR"
            mkdir -p "$CARGO_TARGET_DIR"
            if [ "$SOURCE_IS_TEMP" = "1" ]; then
                echo "  Reusing persistent build cache: $CARGO_TARGET_DIR"
            else
                echo "  Using persistent build cache: $CARGO_TARGET_DIR (SUSI_USE_REPO_TARGET=1 for in-repo target/)"
            fi
        elif [ -n "${SUSI_USE_REPO_TARGET:-}" ]; then
            echo "  Using in-repo target/ (SUSI_USE_REPO_TARGET=1)."
        fi

        # shellcheck disable=SC2086
        (cd "$SCRIPT_DIR" && cargo build --release -p susi --bin susi $LOCKED_FLAG $BUILD_FEATURES)
        stop_heartbeat
        wait_wasm

        ENGINE_SRC="$BUILD_TARGET_DIR/release/susi"

        if [ -f "$ENGINE_SRC" ]; then
            # Validate built binary
            if ! "$ENGINE_SRC" --version >/dev/null 2>&1 && ! "$ENGINE_SRC" --help >/dev/null 2>&1; then
                echo "Error: Built executable validation failed."
                exit 1
            fi
            pkill -f "$GLOBAL_BIN_DIR/susi" || true
            rm -f "$GLOBAL_BIN_DIR/susi" 2>/dev/null || true
            cp "$ENGINE_SRC" "$GLOBAL_BIN_DIR/susi"
            chmod +x "$GLOBAL_BIN_DIR/susi"
            INSTALLED=1
            echo "Deployed susi binary to $GLOBAL_BIN_DIR"
        fi
    fi
fi

if [ "$INSTALLED" = "0" ]; then
    if command -v cargo >/dev/null 2>&1; then
        echo "Error: Installation failed while building from source. Check the cargo build output above for details."
    else
        echo "Error: Installation failed. No pre-built binary is available for $PLATFORM/$ARCH from $SUSI_REPO,"
        echo "and 'cargo' (Rust) is not installed to build from source."
        echo "Install Rust from https://rustup.rs and re-run this installer, or check"
        echo "https://github.com/$SUSI_REPO/releases for a supported binary."
    fi
    exit 1
fi

# 4. Engine Initialization & Sovereign Handshake
if [ -x "$GLOBAL_BIN_DIR/susi" ]; then
    echo "Registering substrate identity and performing hardware audit..."
    "$GLOBAL_BIN_DIR/susi" install
fi

# 5. Daemon Persistence
# susi self-daemonizes on demand: every CLI invocation checks for a running
# daemon and spawns one detached (scoped to the invoking directory) if absent.
# No service registration is needed for that path - the daemon starts the
# first time `susi` runs, which includes this installer's `susi install`
# handshake below. On Linux, when a systemd user manager is reachable, the
# spawn lands in a real cgroup (MemoryMax=2G, CPUQuota=50%) via systemd-run.
#
# SUSI_ALWAYS_ON=1 additionally registers a boot-persistent service
# (systemd user unit / launchd agent) that starts the daemon at login and
# restarts it on exit - opt-in because an always-on background process is a
# resource/UX commitment the user should choose, not a silent default.
# SUSI_NO_DAEMON=1 skips this section entirely (self-daemonization still
# applies at runtime regardless).
if [ -n "$SUSI_NO_DAEMON" ]; then
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
if [[ ":$PATH:" != *":$GLOBAL_BIN_DIR:"* ]]; then
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

# 7. Finalize
if [ -t 0 ] && [ -t 1 ] && [ -z "$NONINTERACTIVE" ] && [ -x "$GLOBAL_BIN_DIR/susi" ]; then
    echo "Installation complete. Starting interactive susi session..."
    echo ""
    exec "$GLOBAL_BIN_DIR/susi"
else
    echo "Installation complete. Run 'susi' to start."
fi
