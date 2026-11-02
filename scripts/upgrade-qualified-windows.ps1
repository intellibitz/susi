# Internal to installed-upgrade. The caller holds the release-sync lock and
# has verified the package evidence against the current qualification receipt.
param(
    [Parameter(Mandatory=$true)][string]$Candidate,
    [Parameter(Mandatory=$true)][ValidatePattern('^[a-f0-9]{64}$')][string]$Digest,
    [Parameter(Mandatory=$true)][ValidatePattern('^[a-f0-9]{40,64}$')][string]$Source
)
$ErrorActionPreference = 'Stop'
$env:PSModulePath = (Join-Path $PSHOME 'Modules') + ';' + $env:PSModulePath
$Root = $env:SUSI_HOME
$BinDir = Join-Path $Root 'bin'
$Bin = Join-Path $BinDir 'susi.exe'
$Marker = Join-Path $BinDir 'susi.build.json'
$PreviousMarker = "$Marker.prev"
$Helper = Join-Path $PSScriptRoot 'susi-release-sync-windows.ps1'
function Hash($Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash }
function Release-Action($Action, $Image) {
    $Arguments = @('-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',$Helper,'-Action',$Action,'-Bin',$Bin)
    if ($Image) { $Arguments += @('-Built',$Image) }
    $Result = & powershell.exe @Arguments
    if ($LASTEXITCODE) { throw "Windows release $Action failed" }
    $Result
}
function Start-HealthyDaemon {
    $Daemon = Start-Process -FilePath $Bin -ArgumentList 'daemon-start' -WorkingDirectory $Root -WindowStyle Hidden -PassThru
    for ($i = 0; $i -lt 60; $i++) {
        if ($Daemon.HasExited) { throw 'Installed daemon exited during startup' }
        try {
            $Response = Invoke-WebRequest -UseBasicParsing -Uri $Health -TimeoutSec 2
            if ($Response.StatusCode -eq 200 -and !$Daemon.HasExited) { return }
        } catch [System.Net.WebException] {}
        Start-Sleep -Seconds 1
    }
    throw 'Installed daemon failed its health probe'
}
if ((Get-Command susi -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source -ne $Bin) { throw 'PATH selects a different installation' }
$OldHash = Hash $Bin
$OldMarker = Get-Content -LiteralPath $Marker -Raw | ConvertFrom-Json
if ($OldMarker.channel -ne 'release') { throw 'Expected an installed release marker' }
$Offset = $env:SUSI_PORT_OFFSET
if (!$Offset -and (Test-Path (Join-Path $Root 'config.json'))) {
    $Offset = (Get-Content (Join-Path $Root 'config.json') -Raw | ConvertFrom-Json).port_offset
}
$Health = "http://127.0.0.1:$(9091 + [int]$Offset)/health"
if ((Hash $Candidate) -ne $Digest) { throw 'Candidate SHA256 mismatch' }
if ($OldHash -eq $Digest) {
    if ($OldMarker.commit -ne $Source) { throw 'Installed bytes match, but release marker differs. Preserve the existing rollback image.' }
    if ((& susi --version) -ne "susi $($OldMarker.version)") { throw 'Installed version and marker disagree' }
    if (!(Release-Action 'running')) { Start-HealthyDaemon }
    if ((Invoke-WebRequest -UseBasicParsing -Uri $Health -TimeoutSec 2).StatusCode -ne 200) { throw 'Installed daemon health check failed' }
    Write-Output 'Exact release already installed and healthy'
    return
}
if (Release-Action 'busy') { throw 'Installed clients are active. Retry during an idle maintenance window.' }
$WasRunning = (Release-Action 'running') -eq 'running'
$Scratch = Join-Path $BinDir ('.qualified-' + [guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($Scratch)
$Swapped = $false
$Attempted = $false
try {
    $Stage = Join-Path $Scratch 'susi.exe'
    Copy-Item -LiteralPath $Candidate -Destination $Stage
    if ((Hash $Stage) -ne $Digest) { throw 'Staged SHA256 mismatch' }
    $Version = & $Stage --version
    if ($LASTEXITCODE -or $Version -notmatch '^susi ([0-9]+\.[0-9]+\.[0-9]+)$') { throw 'Staged version probe failed' }
    $Number = $Matches[1]
    Copy-Item -LiteralPath $Marker -Destination $PreviousMarker -Force
    $Attempted = $true
    Release-Action 'swap' $Stage
    $Swapped = $true
    if ((Hash $Bin) -ne $Digest -or (Hash (Join-Path $BinDir 'susi.prev.exe')) -ne $OldHash) { throw 'Installed or rollback image hash mismatch' }
    $ActualVersion = & susi --version
    if ($LASTEXITCODE -or $ActualVersion -ne $Version) { throw 'PATH version check failed' }
    Start-HealthyDaemon
    $NewMarker = [ordered]@{profile='release';accelerator=$null;channel='release';version=$Number;commit=$Source;installed_at=[DateTimeOffset]::UtcNow.ToUnixTimeSeconds()}
    $MarkerStage = Join-Path $Scratch 'marker.json'
    [IO.File]::WriteAllText($MarkerStage, ($NewMarker | ConvertTo-Json -Compress))
    [IO.File]::Replace($MarkerStage,$Marker,$PreviousMarker)
    Write-Output "$Version installed; SHA256 $Digest; HTTP 200 at $Health"
} catch {
    if ($Swapped) {
        Release-Action 'rollback'
        if ((Hash $Bin) -ne $OldHash) { throw 'Rollback hash mismatch' }
    }
    if ($Attempted -and $WasRunning -and !(Release-Action 'running')) { Start-HealthyDaemon }
    throw
} finally {
    foreach ($Name in 'marker.json','susi.exe') {
        $File = Join-Path $Scratch $Name
        if ([IO.File]::Exists($File)) { [IO.File]::Delete($File) }
    }
    [IO.Directory]::Delete($Scratch)
}
