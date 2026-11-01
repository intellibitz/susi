[CmdletBinding()]
param(
    [ValidateSet('Build','Run','Rollback','Status')][string]$Action = 'Build',
    [ValidateSet('dev','production')][string]$Channel = 'dev',
    [string]$Root = (Join-Path $env:LOCALAPPDATA 'SusiLocal'),
    [string[]]$SusiArgs = @('shell')
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$channelRoot = Join-Path ([IO.Path]::GetFullPath($Root)) $Channel
$statePath = Join-Path $channelRoot 'channel.json'
$state = if (Test-Path -LiteralPath $statePath) { Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json } else { $null }
function Write-State($value) {
    $temporary = "$statePath.$PID.new"
    $value | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $temporary -Encoding UTF8
    Move-Item -LiteralPath $temporary -Destination $statePath -Force
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
    $digest = (Get-FileHash -LiteralPath $state.previous -Algorithm SHA256).Hash.ToLowerInvariant()
    $expected = Split-Path (Split-Path $state.previous -Parent) -Leaf
    if ($expected -notmatch '^[a-f0-9]{64}$' -or $expected -ne $digest) { throw 'Previous local executable checksum changed; rollback refused.' }
    Assert-ChannelIdle
    $version = & $state.previous --version
    if ($LASTEXITCODE -ne 0 -or $version -notmatch '^susi ') { throw 'Previous local executable version check failed; rollback refused.' }
    $previous = $state.current
    $state.current = $state.previous
    $state.previous = $previous
    $state.sha256 = $digest
    $state.version = "$version"
    Write-State $state
    Write-Output "Local $Channel channel rolled back."
    return
}
if ($Action -eq 'Build') {
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $profile = if ($Channel -eq 'dev') { 'dev' } else { 'local-production' }
    $output = if ($Channel -eq 'dev') { 'debug' } else { 'local-production' }
    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo 'target' }
    if (![IO.Path]::IsPathRooted($target)) { $target = Join-Path $repo $target }
    # Seed only from this checkout's existing dev artifacts. No hardlinks,
    # fingerprint rewriting, or cross-worktree workspace objects: Cargo still
    # validates every unit and rebuilds those whose effective profile changed.
    $warm = Join-Path $target 'debug'
    $production = Join-Path $target 'local-production'
    if ($Channel -eq 'production' -and !(Test-Path -LiteralPath $production) -and (Test-Path -LiteralPath $warm)) {
        & robocopy $warm $production /E /COPY:DAT /DCOPY:DAT /XD incremental examples /XF susi.exe 'susi-*.exe' /R:1 /W:1 /NFL /NDL /NJH /NJS /NP | Out-Null
        if ($LASTEXITCODE -ge 8) { throw 'Could not seed the local production cache; no channel was changed.' }
    }
    Push-Location $repo
    try {
        & cargo build --locked --bin susi --profile $profile --timings
        if ($LASTEXITCODE -ne 0) { throw "Local build failed ($LASTEXITCODE); current channel unchanged." }
    } finally { Pop-Location }
    $built = Join-Path $target "$output/susi.exe"
    if (!(Test-Path -LiteralPath $built)) { throw "Expected native Windows executable missing: $built" }
    $digest = (Get-FileHash -LiteralPath $built -Algorithm SHA256).Hash.ToLowerInvariant()
    $slot = Join-Path $channelRoot "builds/$digest"
    New-Item -ItemType Directory -Path $slot -Force | Out-Null
    $candidate = Join-Path $slot 'susi.exe'
    if (!(Test-Path -LiteralPath $candidate)) { Copy-Item -LiteralPath $built -Destination $candidate }
    if ((Get-FileHash -LiteralPath $candidate -Algorithm SHA256).Hash.ToLowerInvariant() -ne $digest) { throw 'Local candidate checksum mismatch.' }
    $version = & $candidate --version
    if ($LASTEXITCODE -ne 0 -or $version -notmatch '^susi ') { throw 'Local candidate version check failed; current channel unchanged.' }
    & (Join-Path $PSScriptRoot 'test-local-channel.ps1') -Binary $candidate
    Assert-ChannelIdle
    Write-State ([ordered]@{ channel=$Channel; current=$candidate; previous=if($state -and $state.current -ne $candidate){$state.current}elseif($state){$state.previous}else{$null}; sha256=$digest; version="$version"; builtUtc=[DateTime]::UtcNow.ToString('o'); buildSeconds=[Math]::Round($watch.Elapsed.TotalSeconds,3) })
    Write-Output "Local $Channel channel ready: $version ($([Math]::Round($watch.Elapsed.TotalSeconds,1)) seconds)."
    Write-Output "Run locally: & '$PSCommandPath' -Action Run -Channel $Channel"
    return
}
if (!$state -or !(Test-Path -LiteralPath $state.current)) { throw "Build the local $Channel channel first." }
if ((Get-FileHash -LiteralPath $state.current -Algorithm SHA256).Hash.ToLowerInvariant() -ne $state.sha256) { throw 'Activated local executable checksum changed; rebuild or roll back before running.' }
$savedHome = $env:SUSI_HOME
$savedPort = $env:SUSI_PORT_OFFSET
try {
    $env:SUSI_HOME = Join-Path $channelRoot 'home'
    $env:SUSI_PORT_OFFSET = if ($Channel -eq 'dev') { '200' } else { '300' }
    & $state.current @SusiArgs
    $result = $LASTEXITCODE
} finally {
    $env:SUSI_HOME = $savedHome
    $env:SUSI_PORT_OFFSET = $savedPort
}
if ($result -ne 0) { throw "Local $Channel Susi exited with code $result." }
