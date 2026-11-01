param(
    [Parameter(Mandatory=$true)][ValidateSet('busy','running','swap','rollback')][string]$Action,
    [Parameter(Mandatory=$true)][string]$Bin,
    [string]$Built
)
$ErrorActionPreference = 'Stop'
# PowerShell 7 callers can pass a module path without Windows PowerShell modules.
$env:PSModulePath = (Join-Path $PSHOME 'Modules') + ';' + $env:PSModulePath
$Bin = [IO.Path]::GetFullPath($Bin)
$BinDir = [IO.Path]::GetDirectoryName($Bin)
$Legacy = Join-Path $BinDir 'susi-engine.exe'
$Previous = Join-Path $BinDir 'susi.prev.exe'
function Owned-Processes {
    $Candidates = @(Get-CimInstance Win32_Process -Filter "Name='susi.exe' OR Name='susi-engine.exe'")
    if ($Candidates | Where-Object { -not $_.ExecutablePath }) {
        throw 'Cannot establish executable ownership for a running Susi process.'
    }
    @($Candidates | Where-Object { $_.ExecutablePath -eq $Bin -or $_.ExecutablePath -eq $Legacy })
}
function Is-Service($Process) {
    # Match the first argument, not words in a user's mission.
    $Process.CommandLine -match '^\s*(?:"[^"]+"|\S+)\s+(?:daemon-start|service-run)(?:\s|$)'
}
$Owned = @(Owned-Processes)
if ($Action -eq 'busy') {
    $Owned | Where-Object { -not (Is-Service $_) } | ForEach-Object { $_.ProcessId }
    exit 0
}
if ($Action -eq 'running') {
    if ($Owned | Where-Object { $_.CommandLine -match '^\s*(?:"[^"]+"|\S+)\s+daemon-start(?:\s|$)' }) { 'running' }
    exit 0
}
if ($Owned | Where-Object { -not (Is-Service $_) }) { throw 'Installed Susi has active clients; retry promotion when idle.' }
$Candidate = if ($Action -eq 'rollback') { $Previous } else { [IO.Path]::GetFullPath($Built) }
if (-not (Test-Path -LiteralPath $Candidate -PathType Leaf)) { throw "Missing release candidate: $Candidate" }
New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
$Staged = "$Bin.new"
Copy-Item -LiteralPath $Candidate -Destination $Staged -Force
if ($Action -eq 'swap' -and (Test-Path -LiteralPath $Bin)) {
    Copy-Item -LiteralPath $Bin -Destination $Previous -Force
}
# Windows locks executing images. Stop only this installation's idle services,
# after staging a complete replacement and preserving the previous executable.
$Owned = @(Owned-Processes)
if ($Owned | Where-Object { -not (Is-Service $_) }) { throw 'A Susi client started during staging; retry when idle.' }
foreach ($Process in $Owned) {
    Stop-Process -Id $Process.ProcessId -ErrorAction Stop
    Wait-Process -Id $Process.ProcessId -Timeout 15 -ErrorAction SilentlyContinue
}
try {
    # Retire the duplicate before publication so cleanup failure cannot leave
    # new executable bytes paired with the old release marker.
    if (Test-Path -LiteralPath $Legacy) { Remove-Item -LiteralPath $Legacy -Force }
    Move-Item -LiteralPath $Staged -Destination $Bin -Force
} catch {
    if (Test-Path -LiteralPath $Previous) {
        Copy-Item -LiteralPath $Previous -Destination $Bin -Force
    }
    throw
}
