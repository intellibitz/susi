# SUSI Windows GPU-aware dev build: `cargo xb --release` with the host's GPU backend
# (cuda / metal) detected by xtask on Windows. Builds into target/ only — it never installs.

$ErrorActionPreference = "Stop"

# Auto-detect CUDA environment on Windows if NVIDIA GPU present
$hasNvidia = $false
try {
    $smi = Get-Command "nvidia-smi.exe" -ErrorAction SilentlyContinue
    if ($smi) {
        $hasNvidia = $true
    }
} catch {}

if ($hasNvidia -and -not $env:CUDARC_CUDA_VERSION) {
    # Default CUDA version for cudarc on Windows if nvcc is present
    try {
        $nvccOut = nvcc --version 2>$null
        if ($nvccOut -match "release (\d+)\.(\d+)") {
            $major = $Matches[1]
            $minor = $Matches[2]
            $env:CUDARC_CUDA_VERSION = "${major}0${minor}0"
        }
    } catch {}
}

cargo xb --release $args
