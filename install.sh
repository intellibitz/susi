#!/usr/bin/env bash
# susi installer - Lightning Fast Intelligence Substrate Onboarding
set -e

GLOBAL_SUSI_DIR="$HOME/.susi"
GLOBAL_BIN_DIR="$GLOBAL_SUSI_DIR/bin"
mkdir -p "$GLOBAL_BIN_DIR"
mkdir -p "$GLOBAL_SUSI_DIR/models"

SUSI_REPO="${SUSI_REPO:-intellibitz/susi}"

# Try to detect repo from git if available
if command -v git >/dev/null 2>&1 && [ -d ".git" ]; then
    GIT_REMOTE=$(git remote get-url origin 2>/dev/null || true)
    if [[ "$GIT_REMOTE" == *"github.com"* ]]; then
        # Extract owner/repo from https://github.com/owner/repo.git or git@github.com:owner/repo.git
        DETECTED_REPO=$(echo "$GIT_REMOTE" | sed -E 's/.*github\.com[:\/](.*)\.git/\1/' | sed -E 's/.*github\.com[:\/](.*)/\1/')
        if [[ -n "$DETECTED_REPO" ]]; then
            SUSI_REPO="$DETECTED_REPO"
        fi
    fi
fi

echo "Initializing susi environment (Repo: $SUSI_REPO)..."

# 1. Detect Environment
OS_TYPE="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH_TYPE="$(uname -m)"

case "$OS_TYPE" in
    linux*)  PLATFORM="linux" ;;
    darwin*) PLATFORM="macos" ;;
    mingw*|cygwin*|msys*) PLATFORM="windows" ;;
    *)       PLATFORM="unknown" ;;
esac

case "$ARCH_TYPE" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *)      ARCH="unknown" ;;
esac

INSTALLED=0

HAS_LOCAL_SOURCE=0
if [ -f "Cargo.toml" ]; then
    HAS_LOCAL_SOURCE=1
elif [[ -n "${BASH_SOURCE[0]}" && -f "$(dirname "${BASH_SOURCE[0]}")/Cargo.toml" ]]; then
    HAS_LOCAL_SOURCE=1
fi

# 2. Try Binary Download First (Lightning Fast) if no local source exists
if [ "$HAS_LOCAL_SOURCE" = "0" ] && [[ "$PLATFORM" != "unknown" && "$ARCH" != "unknown" ]]; then
    # Try downloading both launcher and engine
    GPU_SUFFIX=""
    if [[ "$PLATFORM" == "linux" ]] && command -v nvidia-smi >/dev/null 2>&1; then
        if nvidia-smi --query-gpu=name --format=csv,noheader >/dev/null 2>&1; then
            GPU_SUFFIX="-cuda"
            echo "GPU Environment detected (nvidia-smi). Will request GPU-accelerated binary."
        fi
    fi

    LAUNCHER_BINARY="susi-$PLATFORM-$ARCH"
    ENGINE_BINARY="susi-engine-$PLATFORM-$ARCH$GPU_SUFFIX"

    if [[ "$PLATFORM" == "windows" ]]; then
        LAUNCHER_BINARY="${LAUNCHER_BINARY}.exe"
        ENGINE_BINARY="${ENGINE_BINARY}.exe"
    fi

    BASE_URL="https://github.com/$SUSI_REPO/releases/latest/download"

    echo "Attempting to download pre-compiled binaries from $SUSI_REPO..."

    # --connect-timeout: fail fast if the host is unreachable.
    # --speed-limit/--speed-time (curl) and --timeout (wget, read-timeout):
    # abort on a genuine stall (no bytes for 30s) without capping total
    # transfer time, so a slow-but-progressing download isn't killed early.
    DEPLOYED=0
    if command -v curl >/dev/null 2>&1; then
        # Download Launcher
        echo "  [1/2] Downloading launcher: $LAUNCHER_BINARY..."
        if curl -sSfL --connect-timeout 15 --speed-limit 1024 --speed-time 30 "$BASE_URL/$LAUNCHER_BINARY" -o "$GLOBAL_BIN_DIR/susi-new"; then
            # Download Engine
            echo "  [2/2] Downloading engine: $ENGINE_BINARY..."
            if curl -sSfL --connect-timeout 15 --speed-limit 1024 --speed-time 30 "$BASE_URL/$ENGINE_BINARY" -o "$GLOBAL_BIN_DIR/susi-engine-new"; then
                DEPLOYED=1
            fi
        fi
    elif command -v wget >/dev/null 2>&1; then
        # Download Launcher
        echo "  [1/2] Downloading launcher: $LAUNCHER_BINARY..."
        if wget -q --timeout=30 --tries=2 "$BASE_URL/$LAUNCHER_BINARY" -O "$GLOBAL_BIN_DIR/susi-new"; then
            # Download Engine
            echo "  [2/2] Downloading engine: $ENGINE_BINARY..."
            if wget -q --timeout=30 --tries=2 "$BASE_URL/$ENGINE_BINARY" -O "$GLOBAL_BIN_DIR/susi-engine-new"; then
                DEPLOYED=1
            fi
        fi
    fi

    if [ "$DEPLOYED" = "1" ]; then
        pkill -f susi-engine || true

        BIN_EXE=""
        ENGINE_EXE="-engine"
        if [[ "$PLATFORM" == "windows" ]]; then
            BIN_EXE=".exe"
            ENGINE_EXE="-engine.exe"
        fi

        rm -f "$GLOBAL_BIN_DIR/susi${BIN_EXE}" "$GLOBAL_BIN_DIR/susi${ENGINE_EXE}" 2>/dev/null || true
        mv "$GLOBAL_BIN_DIR/susi-new" "$GLOBAL_BIN_DIR/susi${BIN_EXE}"
        mv "$GLOBAL_BIN_DIR/susi-engine-new" "$GLOBAL_BIN_DIR/susi${ENGINE_EXE}"
        chmod +x "$GLOBAL_BIN_DIR/susi${BIN_EXE}" "$GLOBAL_BIN_DIR/susi${ENGINE_EXE}"
        INSTALLED=1
        echo "Successfully deployed binaries from GitHub ($SUSI_REPO)."
    else
        echo "Binary download unavailable or failed. Falling back to build."
        rm -f "$GLOBAL_BIN_DIR/susi-new" "$GLOBAL_BIN_DIR/susi-engine-new" 2>/dev/null || true
    fi
else
    if [ "$HAS_LOCAL_SOURCE" = "1" ]; then
        echo "Local source repository detected. Skipping remote binary download and building from source."
    fi
fi

# 3. Fallback to Local Source or Clone & Build
if [ "$INSTALLED" = "0" ]; then
    echo "Binary download unavailable or failed. Falling back to build from source..."

    # Detect if we are running from a local file or piped
    SCRIPT_DIR_DETECT=""
    if [[ -n "${BASH_SOURCE[0]}" && -f "${BASH_SOURCE[0]}" ]]; then
        SCRIPT_DIR_DETECT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd 2>/dev/null || echo "")"
    fi

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
    fi

    if command -v cargo >/dev/null 2>&1 && [ -f "$SCRIPT_DIR/Cargo.toml" ]; then
        echo "Building release binaries from source. This crate has a large dependency"
        echo "tree (candle, wasmer, tantivy, tonic, ...) — a cold build with no cargo"
        echo "cache commonly takes 15-30+ minutes. This is expected, not a hang; a"
        echo "heartbeat below confirms the build is still active even during a long"
        echo "silent stretch on a single large crate."

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
        trap stop_heartbeat EXIT

        # Build Engine
        echo "  Building engine..."
        start_heartbeat
        (cd "$SCRIPT_DIR" && cargo build --release $BUILD_FEATURES)
        stop_heartbeat

        # Build Launcher
        echo "  Building launcher..."
        start_heartbeat
        (cd "$SCRIPT_DIR/src/native/susi" && cargo build --release)
        stop_heartbeat

        ENGINE_SRC="$SCRIPT_DIR/target/release/susi-engine"
        LAUNCHER_SRC="$SCRIPT_DIR/src/native/susi/target/release/susi"

        if [[ "$PLATFORM" == "windows" ]]; then
            ENGINE_SRC="${ENGINE_SRC}.exe"
            LAUNCHER_SRC="${LAUNCHER_SRC}.exe"
        fi

        if [ -f "$ENGINE_SRC" ] && [ -f "$LAUNCHER_SRC" ]; then
            pkill -f susi-engine || true
            rm -f "$GLOBAL_BIN_DIR/susi-engine" "$GLOBAL_BIN_DIR/susi" 2>/dev/null || true
            cp "$ENGINE_SRC" "$GLOBAL_BIN_DIR/susi-engine"
            cp "$LAUNCHER_SRC" "$GLOBAL_BIN_DIR/susi"
            chmod +x "$GLOBAL_BIN_DIR/susi-engine" "$GLOBAL_BIN_DIR/susi"
            INSTALLED=1
            echo "Deployed engine and launcher binaries to $GLOBAL_BIN_DIR"
        fi
    fi
fi

if [ "$INSTALLED" = "0" ]; then
    echo "Error: Installation failed. Ensure 'cargo' or 'curl' is available and you have internet access."
    exit 1
fi

# 4. Engine Initialization & Sovereign Handshake
if [ -x "$GLOBAL_BIN_DIR/susi" ]; then
    echo "Registering substrate identity and performing hardware audit..."
    "$GLOBAL_BIN_DIR/susi" install
fi

# 5. Persistence Management (Daemon Auto-Start)
if [[ "$PLATFORM" == "linux" ]]; then
    if command -v systemctl >/dev/null 2>&1 && [ "$EUID" -ne 0 ]; then
        echo "Registering susi daemon with systemd (User Session)..."
        mkdir -p "$HOME/.config/systemd/user"
        cat <<EOF > "$HOME/.config/systemd/user/susi.service"
[Unit]
Description=susi Intelligence Substrate Daemon
After=network.target

[Service]
ExecStart=$GLOBAL_BIN_DIR/susi daemon-start --workspace $HOME
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
EOF
        systemctl --user daemon-reload
        systemctl --user enable susi.service
        systemctl --user start susi.service
    fi
elif [[ "$PLATFORM" == "macos" ]]; then
    echo "Registering susi daemon with launchd..."
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
</dict>
</plist>
EOF
    launchctl load "$LAUNCHD_PLIST" 2>/dev/null || true
fi

# 6. Intelligence Substrate Provisioning (Proof of Life Handshake)
MODEL_DIR="$GLOBAL_SUSI_DIR/models"
REFLEX_MODEL="$MODEL_DIR/susi-alpha.safetensors"
if [ ! -f "$REFLEX_MODEL" ]; then
    echo "Fetching Reflex-Alpha intelligence substrate (Proof of Life)..."
    # The engine's 'install' command already enqueues weights, but we force a tiny fetch for immediate response
    # Using the default HF URL from config if not provided
    WEIGHTS_URL="${SUSI_WEIGHTS_URL:-https://huggingface.co/intellibitz/susi-alpha/resolve/main/susi-alpha.safetensors}"
    if command -v curl >/dev/null 2>&1; then
        curl -sSfL --connect-timeout 15 --speed-limit 1024 --speed-time 30 "$WEIGHTS_URL" -o "$REFLEX_MODEL" || echo "Reflex weights fetch failed or timed out; 'susi install' will retry provisioning in the background."
    elif command -v wget >/dev/null 2>&1; then
        wget -q --timeout=30 --tries=2 "$WEIGHTS_URL" -O "$REFLEX_MODEL" || echo "Reflex weights fetch failed or timed out; 'susi install' will retry provisioning in the background."
    fi
fi

# 7. PATH Management
if [[ ":$PATH:" != *":$GLOBAL_BIN_DIR:"* ]]; then
    CONFIG_FILES=("$HOME/.bashrc" "$HOME/.zshrc" "$HOME/.profile")
    for config in "${CONFIG_FILES[@]}"; do
        if [ -f "$config" ] && ! grep -q ".susi/bin" "$config"; then
            echo -e "\n# susi path initialization\nexport PATH=\"\$HOME/.susi/bin:\$PATH\"" >> "$config"
        fi
    done

    FISH_CONFIG="$HOME/.config/fish/config.fish"
    if [ -d "$HOME/.config/fish" ] || command -v fish >/dev/null 2>&1; then
        mkdir -p "$HOME/.config/fish"
        if [ -f "$FISH_CONFIG" ] && ! grep -q ".susi/bin" "$FISH_CONFIG"; then
            echo -e "\n# susi path initialization\nfish_add_path \$HOME/.susi/bin" >> "$FISH_CONFIG"
        elif [ ! -f "$FISH_CONFIG" ]; then
            echo -e "fish_add_path \$HOME/.susi/bin" > "$FISH_CONFIG"
        fi
        if command -v fish >/dev/null 2>&1; then
            fish -c "fish_add_path $GLOBAL_BIN_DIR" >/dev/null 2>&1 || true
        fi
    fi
fi

# 6. Finalize
if [ -t 0 ] && [ -t 1 ] && [ -z "$NONINTERACTIVE" ] && [ -x "$GLOBAL_BIN_DIR/susi" ]; then
    echo "Installation complete. Starting interactive susi session..."
    echo ""
    exec "$GLOBAL_BIN_DIR/susi"
else
    echo "Installation complete. Run 'susi' to start."
fi
