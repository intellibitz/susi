param(
    [switch]$Service = $false
)

# SUSI Micro-Substrate Installer for Windows
# Tier 0 Onboarding Protocol: Zero-Config Deployment

$SusiRepo = "https://github.com/intellibitz/susi"
$ReleaseUrl = "$SusiRepo/releases/latest/download"
$HomeDir = [Environment]::GetFolderPath("UserProfile")
$GlobalDir = Join-Path $HomeDir ".susi"
$GlobalBinDir = Join-Path $GlobalDir "bin"

# Ensure substrate structure exists
if (-not (Test-Path $GlobalBinDir)) {
    New-Item -ItemType Directory -Force -Path $GlobalBinDir | Out-Null
}

Write-Host "[susi] Initializing 100% Sandboxed Native AI Runtime (Repo: $SusiRepo)..." -ForegroundColor Cyan

$LauncherExePath = Join-Path $GlobalBinDir "susi.exe"
$EngineExePath = Join-Path $GlobalBinDir "susi-engine.exe"

$Installed = $false

# 1. Try Binary Download First (Lightning Fast)
if ($null -eq $env:LOCAL_SOURCE) {
    Write-Host "🔐 Fetching latest susi engine..." -ForegroundColor Yellow

    # The release publishes `susi-windows-x86_64.exe` (+ .sha256), the same
    # `<product>-<platform>-<arch>` naming install.sh resolves on Unix.
    $EngineBinary = "susi-windows-x86_64.exe"
    $EngineTmpPath = Join-Path $GlobalBinDir "susi-engine-new.exe"
    $ChecksumTmpPath = "$EngineTmpPath.sha256"

    Stop-Process -Name "susi" -ErrorAction SilentlyContinue
    Stop-Process -Name "susi-engine" -ErrorAction SilentlyContinue

    try {
        Invoke-WebRequest -Uri "$ReleaseUrl/$EngineBinary" -OutFile $EngineTmpPath -UseBasicParsing
        Invoke-WebRequest -Uri "$ReleaseUrl/$EngineBinary.sha256" -OutFile $ChecksumTmpPath -UseBasicParsing

        # Never run a downloaded binary without verifying it against the
        # published checksum first - a missing/mismatched checksum falls
        # back to a source build rather than executing an unverified binary.
        $ExpectedSha = (Get-Content $ChecksumTmpPath -Raw).Trim().Split(" ")[0].ToLower()
        $ActualSha = (Get-FileHash $EngineTmpPath -Algorithm SHA256).Hash.ToLower()
        Remove-Item $ChecksumTmpPath -Force -ErrorAction SilentlyContinue

        if ($ExpectedSha -and ($ActualSha -eq $ExpectedSha)) {
            Move-Item $EngineTmpPath $EngineExePath -Force
            # `susi.exe` is not a separate binary - susi-engine's own CLI already
            # accepts "susi" as its command name and handles every subcommand
            # directly. A real copy (not a symlink) since NTFS symlinks need
            # elevated privileges/Developer Mode that a fresh install can't assume.
            Copy-Item $EngineExePath $LauncherExePath -Force
            $Installed = $true
            Write-Host "  ✅ Downloaded & verified the engine binary from GitHub." -ForegroundColor Green
        } else {
            Remove-Item $EngineTmpPath -Force -ErrorAction SilentlyContinue
            Write-Host "  ⚠️ Checksum verification failed for $EngineBinary. Discarding download and falling back to source build." -ForegroundColor Yellow
        }
    } catch {
        Remove-Item $EngineTmpPath -Force -ErrorAction SilentlyContinue
        Remove-Item $ChecksumTmpPath -Force -ErrorAction SilentlyContinue
        Write-Host "  ⚠️ Binary download failed. Falling back to source build." -ForegroundColor Yellow
    }
}

# 2. Fallback to Source Build
if (-not $Installed -and (Get-Command "cargo" -ErrorAction SilentlyContinue)) {
    Write-Host "[susi Native] Compiling standalone Rust AI engine..." -ForegroundColor Yellow

    Stop-Process -Name "susi" -ErrorAction SilentlyContinue
    Stop-Process -Name "susi-engine" -ErrorAction SilentlyContinue

    # Match install.sh: thin-LTO release profile + only the installable binary.
    # sccache when present; --locked when Cargo.lock ships with the tree.
    if ((Get-Command "sccache" -ErrorAction SilentlyContinue) -and -not $env:RUSTC_WRAPPER) {
        $env:RUSTC_WRAPPER = "sccache"
        Write-Host "  Using sccache for faster recompiles." -ForegroundColor Gray
    }

    Write-Host "  Building engine..." -ForegroundColor Gray
    $locked = @()
    if (Test-Path "Cargo.lock") { $locked = @("--locked") }
    & cargo build --release -p susi --bin susi @locked
    if ($LASTEXITCODE -ne 0) {
        Write-Error "cargo build failed with exit code $LASTEXITCODE"
        exit $LASTEXITCODE
    }

    $EngineSrc = "target\release\susi.exe"
    if (-not (Test-Path $EngineSrc)) {
        # Legacy artifact name from older trees
        $EngineSrc = "target\release\susi-engine.exe"
    }

    if (Test-Path $EngineSrc) {
        Copy-Item $EngineSrc $EngineExePath -Force
        Copy-Item $EngineExePath $LauncherExePath -Force
        $Installed = $true
        Write-Host "  ✅ Compiled & installed the engine binary." -ForegroundColor Green
    }
}

if (-not $Installed) {
    if (Get-Command "cargo" -ErrorAction SilentlyContinue) {
        Write-Error "Installation failed while building from source. Check the cargo build output above for details."
    } else {
        Write-Error "Installation failed. No verified pre-built binary is available from $SusiRepo, and 'cargo' (Rust) is not installed to build from source. Install Rust from https://rustup.rs and re-run this installer."
    }
    exit 1
}

# 3. PATH Automation (0-Effort Onboarding)
$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($UserPath -notlike "*$GlobalBinDir*") {
    Write-Host "[susi] Automatically adding '$GlobalBinDir' to User PATH..." -ForegroundColor Cyan
    [Environment]::SetEnvironmentVariable("Path", "$GlobalBinDir;$UserPath", "User")
    $env:Path = "$GlobalBinDir;$env:Path"
    Write-Host "  ✅ User PATH updated!" -ForegroundColor Green
}

# 4. Optional Service Registration
if ($Service) {
    Write-Host "[susi] Registering Windows Scheduled Task for background daemon service..." -ForegroundColor Cyan
    try {
        $TaskName = "susi-daemon"
        $Action = New-ScheduledTaskAction -Execute $LauncherExePath -Argument "daemon"
        $Trigger = New-ScheduledTaskTrigger -AtLogOn
        $Principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -RunLevel Highest
        Register-ScheduledTask -TaskName $TaskName -Action $Action -Trigger $Trigger -Principal $Principal -Force | Out-Null
        Write-Host "  ✅ Registered scheduled task '$TaskName' to run at logon with highest privileges." -ForegroundColor Green
    } catch {
        Write-Host "  ⚠️ Could not register scheduled task: $_" -ForegroundColor Yellow
    }
}

# 5. Non-Interactive Elevation Optimization
if (Get-Command "gsudo" -ErrorAction SilentlyContinue) {
    try {
        gsudo config CacheMode Auto | Out-Null
        gsudo config CacheDuration Infinite | Out-Null
        Write-Host "  ✅ Configured gsudo credential caching for non-interactive elevation." -ForegroundColor Green
    } catch {}
}

Write-Host "`n🎉 Global susi is ready! Type 'susi status' or 'susi mcp' to verify." -ForegroundColor Green
Write-Host "Restart your terminal to use the 'susi' command." -ForegroundColor Gray
