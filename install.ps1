param(
    [switch]$Service = $false,
    [switch]$NoLaunch = $false
)

$ErrorActionPreference = 'Stop'

# SUSI Micro-Substrate Installer for Windows
# Tier 0 Onboarding Protocol: Zero-Config Deployment

$SusiRepo = "https://github.com/intellibitz/susi"
$ReleaseUrl = "$SusiRepo/releases/latest/download"
$HomeDir = [Environment]::GetFolderPath("UserProfile")
$IsolatedInstall = -not [string]::IsNullOrWhiteSpace($env:SUSI_HOME)
if ($IsolatedInstall -and -not [IO.Path]::IsPathRooted($env:SUSI_HOME)) {
    throw 'SUSI_HOME must be an absolute installation path.'
}
$GlobalDir = if ($IsolatedInstall) { [IO.Path]::GetFullPath($env:SUSI_HOME) } else { Join-Path $HomeDir ".susi" }
$GlobalBinDir = Join-Path $GlobalDir "bin"

# Ensure substrate structure exists
if (-not (Test-Path $GlobalBinDir)) {
    New-Item -ItemType Directory -Force -Path $GlobalBinDir | Out-Null
}

Write-Host "[susi] Installing your local and cloud AI operating layer..." -ForegroundColor Cyan

$LauncherExePath = Join-Path $GlobalBinDir "susi.exe"
$EngineExePath = $LauncherExePath

# Stop only the installation being replaced, never another user's or test instance.
function Stop-InstalledSusi {
    Get-Process -Name susi,susi-engine -ErrorAction SilentlyContinue | ForEach-Object {
        if ($_.Path -and [IO.Path]::GetDirectoryName($_.Path) -eq $GlobalBinDir) {
            Stop-Process -Id $_.Id -ErrorAction Stop
        }
    }
}

$Installed = $false

# 1. Try Binary Download First (Lightning Fast)
if ($null -eq $env:LOCAL_SOURCE) {
    Write-Host "🔐 Fetching latest susi engine..." -ForegroundColor Yellow

    # The release publishes `susi-windows-x86_64.exe` (+ .sha256), the same
    # `<product>-<platform>-<arch>` naming install.sh resolves on Unix.
    $EngineBinary = "susi-windows-x86_64.exe"
    $EngineTmpPath = Join-Path $GlobalBinDir "susi-engine-new.exe"
    $ChecksumTmpPath = "$EngineTmpPath.sha256"


    try {
        Invoke-WebRequest -Uri "$ReleaseUrl/$EngineBinary" -OutFile $EngineTmpPath -UseBasicParsing -TimeoutSec 120
        Invoke-WebRequest -Uri "$ReleaseUrl/$EngineBinary.sha256" -OutFile $ChecksumTmpPath -UseBasicParsing -TimeoutSec 30

        # Never run a downloaded binary without verifying it against the
        # published checksum first - a missing/mismatched checksum falls
        # back to a source build rather than executing an unverified binary.
        $ExpectedSha = (Get-Content $ChecksumTmpPath -Raw).Trim().Split(" ")[0].ToLower()
        $ActualSha = (Get-FileHash $EngineTmpPath -Algorithm SHA256).Hash.ToLower()
        Remove-Item $ChecksumTmpPath -Force -ErrorAction SilentlyContinue

        if ($ExpectedSha -and ($ActualSha -eq $ExpectedSha)) {
            Stop-InstalledSusi
            Move-Item $EngineTmpPath $EngineExePath -Force
            # One canonical executable; no redundant full-size launcher copy.
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
        Stop-InstalledSusi
        Copy-Item $EngineSrc $EngineExePath -Force
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
if (-not $IsolatedInstall -and $GlobalBinDir -notin ($UserPath -split ';')) {
    Write-Host "[susi] Automatically adding '$GlobalBinDir' to User PATH..." -ForegroundColor Cyan
    [Environment]::SetEnvironmentVariable("Path", "$GlobalBinDir;$UserPath", "User")
    Write-Host "  ✅ User PATH updated!" -ForegroundColor Green
}
if ($GlobalBinDir -notin ($env:Path -split ';')) { $env:Path = "$GlobalBinDir;$env:Path" }

# 4. Optional Service Registration
if ($Service -and -not $IsolatedInstall) {
    Write-Host "[susi] Registering Windows Scheduled Task for background daemon service..." -ForegroundColor Cyan
    try {
        $TaskName = "susi-daemon"
        $Action = New-ScheduledTaskAction -Execute $LauncherExePath -Argument "start"
        $Trigger = New-ScheduledTaskTrigger -AtLogOn
        $Principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -RunLevel Limited
        Register-ScheduledTask -TaskName $TaskName -Action $Action -Trigger $Trigger -Principal $Principal -Force | Out-Null
        Write-Host "  Registered Susi to start at logon." -ForegroundColor Green
    } catch {
        Write-Host "  ⚠️ Could not register scheduled task: $_" -ForegroundColor Yellow
    }
}

# Opening accepts a goal immediately; discovery must not delay the prompt.
# Headless installs can opt out, without making a false claim of model readiness.
Write-Host "`nSusi is installed. No terminal restart is needed." -ForegroundColor Green
if (-not $NoLaunch -and $env:NONINTERACTIVE -ne '1' -and $env:SUSI_NO_DAEMON -ne '1' -and [Environment]::UserInteractive) {
    & $LauncherExePath shell
    if ($LASTEXITCODE -ne 0) { throw "Susi opening failed (exit $LASTEXITCODE)." }
} else {
    Write-Host "Open Susi and give it a goal: $LauncherExePath" -ForegroundColor Gray
}
