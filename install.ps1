param(
    [switch]$Service = $false,
    [switch]$NoLaunch = $false
)

$ErrorActionPreference = 'Stop'

# Release scope (CEO decision 2026-11-03): the free public core release is
# published for x86_64 Windows and x86_64 Linux only. ARM64 Windows support
# exists in the source, but core publishes no ARM64 binary; certified builds
# for other platforms come from the Susi commercial suite. Refuse before any
# download, build or write. The native OS architecture is read three ways
# because an x64 PowerShell emulated on ARM64 reports AMD64 for its own
# process; any one naming ARM64 refuses.
function Get-SusiNativeArchitectures {
    $names = @($env:PROCESSOR_ARCHITECTURE, $env:PROCESSOR_ARCHITEW6432)
    try {
        $names += [string][System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    } catch { }
    try {
        $names += (Get-ItemProperty -Path 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment' -Name PROCESSOR_ARCHITECTURE -ErrorAction Stop).PROCESSOR_ARCHITECTURE
    } catch { }
    $names | Where-Object { $_ }
}
if (@(Get-SusiNativeArchitectures | Where-Object { $_ -match '^arm64$' }).Count -gt 0) {
    Write-Host "[susi] This is ARM64 Windows. The free Susi core release supports x86_64 Windows and x86_64 Linux only." -ForegroundColor Red
    Write-Host "[susi] Other platforms, including ARM64 Windows, are served by the Susi commercial suite. Nothing was installed." -ForegroundColor Red
    # throw, like the SUSI_HOME check below: `exit` would also close a
    # session that ran this script through Invoke-Expression.
    throw 'Unsupported platform for the free Susi core release: ARM64 Windows.'
}

# SUSI installer for Windows x86_64: builds the newest release tag from source.
#
# A release is a git tag plus its committed record releases/<tag>.json
# (docs/releases.md). This script resolves the tag with `git ls-remote --tags`
# against SUSI_SOURCE_REPO (or the origin of the checkout it runs from), checks
# it out, builds it with the recorded settings and the toolchain pinned in
# rust-toolchain.toml, and installs it only when it reports the tag's version.
# It downloads no binary and installs no system software.

$HomeDir = [Environment]::GetFolderPath("UserProfile")
$IsolatedInstall = -not [string]::IsNullOrWhiteSpace($env:SUSI_HOME)
if ($IsolatedInstall -and -not [IO.Path]::IsPathRooted($env:SUSI_HOME)) {
    throw 'SUSI_HOME must be an absolute installation path.'
}
$GlobalDir = if ($IsolatedInstall) { [IO.Path]::GetFullPath($env:SUSI_HOME) } else { Join-Path $HomeDir ".susi" }
$GlobalBinDir = Join-Path $GlobalDir "bin"

# Requirements, checked before anything is written. A missing tool stops the
# install with what to install; nothing is installed on the user's behalf.
foreach ($Tool in 'git', 'cargo', 'rustup') {
    if (-not (Get-Command $Tool -ErrorAction SilentlyContinue)) {
        if ($Tool -eq 'git') {
            throw 'git is required to fetch the Susi release tag. Install Git for Windows and re-run.'
        }
        throw "$Tool was not found. Susi installs by building its release tag from source, which needs a Rust toolchain managed by rustup (https://rustup.rs). Install rustup, then re-run; the release pins its compiler in rust-toolchain.toml."
    }
}

# Native-command output with stderr discarded: under 'Stop', Windows PowerShell
# 5.1 turns redirected stderr lines into terminating errors.
function Get-Quiet {
    $Previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $Out = & $args[0] @($args | Select-Object -Skip 1) 2>$null
        [pscustomobject]@{ Code = $LASTEXITCODE; Out = @($Out) }
    } finally {
        $ErrorActionPreference = $Previous
    }
}

# The source repository: SUSI_SOURCE_REPO, else the origin of the checkout this
# script runs from. No host name is built in, and the caller's working
# directory never chooses the source.
$SourceRepo = $env:SUSI_SOURCE_REPO
if ([string]::IsNullOrWhiteSpace($SourceRepo) -and $PSScriptRoot) {
    $Manifest = Join-Path $PSScriptRoot 'Cargo.toml'
    if ((Test-Path -LiteralPath $Manifest) -and (Select-String -LiteralPath $Manifest -Pattern '^name *= *"susi"' -Quiet)) {
        $SourceRepo = ((Get-Quiet git -C $PSScriptRoot remote get-url origin).Out -join '').Trim()
    }
}
if ([string]::IsNullOrWhiteSpace($SourceRepo)) {
    throw 'No source repository. Set SUSI_SOURCE_REPO to the git URL of the Susi core repository, or run install.ps1 from a clone of it.'
}

function Invoke-Git {
    & git @args
    if ($LASTEXITCODE -ne 0) { throw "git $($args -join ' ') failed (exit $LASTEXITCODE)." }
}

# Ensure substrate structure exists
if (-not (Test-Path $GlobalBinDir)) {
    New-Item -ItemType Directory -Force -Path $GlobalBinDir | Out-Null
}

Write-Host "[susi] Installing your local and cloud AI operating layer (source: $SourceRepo)..." -ForegroundColor Cyan

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

# 1. Resolve the release: the newest vX.Y.Z tag, or SUSI_RELEASE_TAG.
$Tag = $env:SUSI_RELEASE_TAG
if ([string]::IsNullOrWhiteSpace($Tag)) {
    $Tags = @(& git ls-remote --tags --refs $SourceRepo 'v*' | ForEach-Object { ($_ -split "`t")[1] -replace '^refs/tags/', '' } |
        Where-Object { $_ -match '^v\d+\.\d+\.\d+$' })
    $Tag = $Tags | Sort-Object { [version]($_.Substring(1)) } | Select-Object -Last 1
}
if ($Tag -notmatch '^v\d+\.\d+\.\d+$') {
    throw "No release tag vX.Y.Z found on $SourceRepo (got '$Tag')."
}
$Version = $Tag.Substring(1)

# 2. Check the tag out into the private release checkout the upgrade script
# (scripts/susi-release-sync.sh) also uses, and read its record from the
# repository's default branch.
$Src = Join-Path $GlobalDir 'release-src'
if (-not (Test-Path -LiteralPath (Join-Path $Src '.git'))) {
    Write-Host "  Cloning $SourceRepo into $Src..." -ForegroundColor Gray
    Invoke-Git clone --quiet --no-checkout $SourceRepo $Src
}
Invoke-Git -C $Src remote set-url origin $SourceRepo
Invoke-Git -C $Src fetch --quiet --force origin "refs/tags/${Tag}:refs/tags/${Tag}"
Invoke-Git -C $Src fetch --quiet origin HEAD
$Shown = Get-Quiet git -C $Src show "FETCH_HEAD:releases/$Tag.json"
$RecordText = $Shown.Out -join "`n"
if ($Shown.Code -ne 0 -or [string]::IsNullOrWhiteSpace($RecordText)) {
    throw "$Tag has no release record releases/$Tag.json on $SourceRepo; it is not a release (docs/releases.md)."
}
$Record = $RecordText | ConvertFrom-Json
$Commit = (& git -C $Src rev-parse "refs/tags/$Tag^{commit}").Trim()
if ($Record.commit -ne $Commit -or $Record.tag -ne $Tag) {
    throw "$Tag points at $Commit, but releases/$Tag.json records '$($Record.commit)'. Refusing to build."
}
Invoke-Git -C $Src -c advice.detachedHead=false checkout --quiet --force --detach "refs/tags/$Tag"
Invoke-Git -C $Src clean --quiet -ffdx

# 3. The pinned compiler. rustup must already have it.
$Channel = ''
$ToolchainFile = Join-Path $Src 'rust-toolchain.toml'
if (Test-Path -LiteralPath $ToolchainFile) {
    $Match = Select-String -LiteralPath $ToolchainFile -Pattern '^channel *= *"([^"]+)"' | Select-Object -First 1
    if ($Match) { $Channel = $Match.Matches[0].Groups[1].Value }
}
if ($Channel -and -not ((Get-Quiet rustup toolchain list).Out | Where-Object { $_ -match "^$([regex]::Escape($Channel))[-\s]" })) {
    throw "$Tag pins Rust $Channel (rust-toolchain.toml), which rustup does not have. Install it, then re-run: rustup toolchain install $Channel --profile minimal"
}

# 4. Build with the recorded profile, features and environment.
$Safe = '^[A-Za-z0-9_./,-]*$'
$BuildProfile = [string]$Record.build.profile
$Features = [string]$Record.build.features
if (-not $BuildProfile -or $BuildProfile -notmatch $Safe -or $Features -notmatch $Safe) {
    throw "releases/$Tag.json has no usable build profile or features."
}
$CargoArgs = @('build', '--locked', '--profile', $BuildProfile, '-p', 'susi', '--bin', 'susi')
if ($Features) { $CargoArgs += @('--features', $Features) }
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $GlobalDir 'build-cache\release' }
New-Item -ItemType Directory -Force -Path $TargetDir | Out-Null
$Saved = @{}
$BuildEnv = @{ CARGO_TARGET_DIR = $TargetDir; RUSTUP_AUTO_INSTALL = '0' }
foreach ($Entry in @($Record.build.env)) {
    if ($Entry -notmatch '^([A-Z_][A-Z0-9_]*)=([A-Za-z0-9_./,-]*)$') { throw "releases/$Tag.json has an unusable build.env entry: $Entry" }
    $BuildEnv[$Matches[1]] = $Matches[2]
}
if ((Get-Command "sccache" -ErrorAction SilentlyContinue) -and -not $env:RUSTC_WRAPPER) {
    $BuildEnv['RUSTC_WRAPPER'] = 'sccache'
    Write-Host "  Using sccache for faster recompiles." -ForegroundColor Gray
}
Write-Host "[susi] Building susi $Tag ($Commit) with Rust $Channel`: cargo $($CargoArgs -join ' ') (a cold build takes several minutes)..." -ForegroundColor Yellow
foreach ($Name in $BuildEnv.Keys) {
    $Saved[$Name] = [Environment]::GetEnvironmentVariable($Name, 'Process')
    [Environment]::SetEnvironmentVariable($Name, $BuildEnv[$Name], 'Process')
}
Push-Location $Src
try {
    & cargo @CargoArgs
    $BuildExit = $LASTEXITCODE
} finally {
    Pop-Location
    foreach ($Name in $Saved.Keys) { [Environment]::SetEnvironmentVariable($Name, $Saved[$Name], 'Process') }
}
if ($BuildExit -ne 0) {
    throw "cargo build failed with exit code $BuildExit. Nothing was installed."
}
$OutDir = switch ($BuildProfile) { 'dev' { 'debug' } 'test' { 'debug' } 'bench' { 'release' } default { $BuildProfile } }
$EngineSrc = Join-Path $TargetDir "$OutDir\susi.exe"
if (-not (Test-Path -LiteralPath $EngineSrc)) {
    throw "The build produced no $EngineSrc. Nothing was installed."
}

# 5. The gate: the built binary must report the tag's version.
$SavedHome = $env:SUSI_HOME
$env:SUSI_HOME = $GlobalDir
try { $Reported = (((Get-Quiet $EngineSrc --version).Out -join ' ') -split '\s+')[1] } finally { $env:SUSI_HOME = $SavedHome }
if ($Reported -ne $Version) {
    throw "The built binary reports version '$Reported', expected '$Version'. Nothing was installed."
}
# Informational: compare with the reference build's checksum. Most independent
# builds differ in bytes (reproducibility is not claimed), so a difference is
# reported, never enforced.
$Asset = 'susi-windows-x86_64.exe'
$Want = ($Record.assets | Where-Object { $_.file -eq $Asset } | Select-Object -First 1).sha256
$Have = (Get-FileHash -LiteralPath $EngineSrc -Algorithm SHA256).Hash.ToLower()
if (-not $Want) {
    Write-Host "  checksum: $Have (the release records no reference build for $Asset)" -ForegroundColor Gray
} elseif ($Want -eq $Have) {
    Write-Host "  checksum: $Have matches the reference build" -ForegroundColor Gray
} else {
    Write-Host "  checksum: $Have differs from the reference build $Want, as expected for an independent build (docs/releases.md)" -ForegroundColor Gray
}

Stop-InstalledSusi
Copy-Item -LiteralPath $EngineSrc -Destination $EngineExePath -Force
Write-Host "  Installed susi $Version." -ForegroundColor Green

# Older installations kept a second full copy. The verified replacement is
# already installed and the old instance stopped, so retire only that copy.
$LegacyEnginePath = Join-Path $GlobalBinDir "susi-engine.exe"
if (Test-Path -LiteralPath $LegacyEnginePath -PathType Leaf) {
    Remove-Item -LiteralPath $LegacyEnginePath -Force
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

# Core has no interactive prompt: it takes one-shot goals, and the conversation
# lives in the Susi desktop app. Interactive installs show usage once; headless
# installs can opt out, without making a false claim of model readiness.
Write-Host "`nSusi is installed. No terminal restart is needed." -ForegroundColor Green
if (-not $NoLaunch -and $env:NONINTERACTIVE -ne '1' -and $env:SUSI_NO_DAEMON -ne '1' -and [Environment]::UserInteractive) {
    & $LauncherExePath --help
    if ($LASTEXITCODE -ne 0) { throw "Susi usage check failed (exit $LASTEXITCODE)." }
}
Write-Host "Open Susi and give it a goal: $LauncherExePath `"<what you want done>`"" -ForegroundColor Gray
