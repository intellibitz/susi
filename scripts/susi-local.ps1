[CmdletBinding()]
param(
    [ValidateSet('Build','Run','Rollback','Status')][string]$Action = 'Build',
    [ValidateSet('dev','production')][string]$Channel = 'dev',
    [string]$Root = (Join-Path $env:LOCALAPPDATA 'SusiLocal'),
    [string]$ShortcutDirectory,
    [string[]]$SusiArgs = @('shell')
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$channelRoot = Join-Path ([IO.Path]::GetFullPath($Root)) $Channel
$statePath = Join-Path $channelRoot 'channel.json'
$state = if (Test-Path -LiteralPath $statePath) { Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json } else { $null }
function Write-State($value) {
    $temporary = "$statePath.$PID.new"
    [IO.File]::WriteAllText($temporary, ($value | ConvertTo-Json -Depth 5), (New-Object Text.UTF8Encoding($false)))
    Move-Item -LiteralPath $temporary -Destination $statePath -Force
}
function Get-CandidateVersion([string]$Binary) {
    $savedHome = $env:SUSI_HOME
    try {
        # Candidate/rollback slots are not active yet. Version introspection
        # explicitly uses an isolated instance; normal launches stay strict.
        $env:SUSI_HOME = Join-Path $channelRoot 'version-check'
        $version = & $Binary --version
        if ($LASTEXITCODE -ne 0 -or $version -notmatch '^susi ') { throw 'Local executable version check failed.' }
        return "$version"
    } finally { $env:SUSI_HOME = $savedHome }
}
function Assert-ChannelIdle {
    $prefix = (Join-Path $channelRoot 'builds') + [IO.Path]::DirectorySeparatorChar
    $running = @(Get-CimInstance Win32_Process -Filter "Name = 'susi.exe'" | Where-Object {
        $_.ExecutablePath -and $_.ExecutablePath.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)
    })
    if ($running.Count) { throw 'The local channel is running. Finish its goals and stop its daemon before switching builds.' }
}
if ($Action -eq 'Status') { $state | ConvertTo-Json -Depth 5; return }
if ($Action -eq 'Rollback') {
    if (!$state -or !$state.previous -or !(Test-Path -LiteralPath $state.previous)) { throw 'No previous local build is available.' }
    if ($state.previous_native_channel -ne $true) { throw 'Previous build predates native channel isolation; rollback refused and current channel preserved.' }
    $digest = (Get-FileHash -LiteralPath $state.previous -Algorithm SHA256).Hash.ToLowerInvariant()
    $expected = Split-Path (Split-Path $state.previous -Parent) -Leaf
    if ($expected -notmatch '^[a-f0-9]{64}$' -or $expected -ne $digest) { throw 'Previous local executable checksum changed; rollback refused.' }
    Assert-ChannelIdle
    $version = Get-CandidateVersion $state.previous
    $previous = $state.current
    $previousNative = $state.native_channel
    $state.current = $state.previous
    $state.previous = $previous
    $state.native_channel = $true
    $state.previous_native_channel = $previousNative
    $state.sha256 = $digest
    $state.version = "$version"
    & (Join-Path $PSScriptRoot 'susi-local-launch.ps1') -Action Install -Channel $Channel -Root $Root -Candidate $state.current -ShortcutDirectory $ShortcutDirectory
    Write-State $state
    Write-Output "Local $Channel channel rolled back."
    return
}
if ($Action -eq 'Build') {
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo 'target' }
    if (![IO.Path]::IsPathRooted($target)) { $target = Join-Path $repo $target }
    # Reuse the same worktree's native dependency cache. Cargo fingerprints
    # the changed workspace optimization and keeps both sets of artifacts;
    # no duplicated target, copied fingerprints, or global compiler settings.
    $settings = @{
        CARGO_PROFILE_DEV_OPT_LEVEL = $(if ($Channel -eq 'production') { '1' } else { '0' })
        CARGO_PROFILE_DEV_LTO = 'false'
        CARGO_PROFILE_DEV_INCREMENTAL = 'true'
        RUSTC_WRAPPER = $null
    }
    $savedSettings = @{}
    foreach ($key in $settings.Keys) {
        $savedSettings[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
        [Environment]::SetEnvironmentVariable($key, $settings[$key], 'Process')
    }
    Push-Location $repo
    try {
        & cargo --config "build.rustc-wrapper=''" build --locked --bin susi --profile dev --timings
        if ($LASTEXITCODE -ne 0) { throw "Local build failed ($LASTEXITCODE); current channel unchanged." }
    } finally {
        Pop-Location
        foreach ($key in $settings.Keys) { [Environment]::SetEnvironmentVariable($key, $savedSettings[$key], 'Process') }
    }
    $built = Join-Path $target 'debug/susi.exe'
    if (!(Test-Path -LiteralPath $built)) { throw "Expected native Windows executable missing: $built" }
    $digest = (Get-FileHash -LiteralPath $built -Algorithm SHA256).Hash.ToLowerInvariant()
    $slot = Join-Path $channelRoot "builds/$digest"
    New-Item -ItemType Directory -Path $slot -Force | Out-Null
    $candidate = Join-Path $slot 'susi.exe'
    if (!(Test-Path -LiteralPath $candidate)) { Copy-Item -LiteralPath $built -Destination $candidate }
    if ((Get-FileHash -LiteralPath $candidate -Algorithm SHA256).Hash.ToLowerInvariant() -ne $digest) { throw 'Local candidate checksum mismatch.' }
    $version = Get-CandidateVersion $candidate
    & (Join-Path $PSScriptRoot 'test-local-channel.ps1') -Binary $candidate
    Assert-ChannelIdle
    & (Join-Path $PSScriptRoot 'susi-local-launch.ps1') -Action Install -Channel $Channel -Root $Root -Candidate $candidate -ShortcutDirectory $ShortcutDirectory
    Write-State ([ordered]@{ channel=$Channel; current=$candidate; previous=if($state -and $state.current -ne $candidate){$state.current}elseif($state){$state.previous}else{$null}; native_channel=$true; previous_native_channel=if($state -and $state.current -ne $candidate){$state.native_channel -eq $true}elseif($state){$state.previous_native_channel -eq $true}else{$false}; sha256=$digest; version="$version"; builtUtc=[DateTime]::UtcNow.ToString('o'); buildSeconds=[Math]::Round($watch.Elapsed.TotalSeconds,3) })
    Write-Output "Local $Channel channel ready: $version ($([Math]::Round($watch.Elapsed.TotalSeconds,1)) seconds)."
    Write-Output 'Ready to open from the Start Menu.'
    return
}
if (!$state -or !(Test-Path -LiteralPath $state.current)) { throw "Build the local $Channel channel first." }
if ((Get-FileHash -LiteralPath $state.current -Algorithm SHA256).Hash.ToLowerInvariant() -ne $state.sha256) { throw 'Activated local executable checksum changed; rebuild or roll back before running.' }
& $state.current @SusiArgs
if ($LASTEXITCODE -ne 0) { throw "Local $Channel Susi exited with code $LASTEXITCODE." }
