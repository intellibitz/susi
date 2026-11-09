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
function Is-Daemon($Process) {
    $Process.CommandLine -match '^\s*(?:"[^"]+"|\S+)\s+daemon-start(?:\s|$)'
}
# A listed process is gone when no process holds its PID any more, or the PID
# now names a process created after it: Windows reuses a PID only once the
# process that held it has exited and every handle to it is closed.
function Is-Gone($Process) {
    $Now = @(Get-CimInstance Win32_Process -Filter "ProcessId=$($Process.ProcessId)")
    $Now.Count -eq 0 -or $Now[0].CreationDate -ne $Process.CreationDate
}
# Stops one process listed by Owned-Processes and waits, bounded, for it to
# exit. Each leaf service runs in the daemon's kill-on-close job, so stopping
# the daemon takes its leaves down too: a listed process that has already
# exited counts as stopped. Any other failure, such as access denied to a
# process that is still running, fails the promotion.
function Stop-Owned($Process) {
    try {
        $Live = Get-Process -Id $Process.ProcessId -ErrorAction Stop
    } catch [Microsoft.PowerShell.Commands.ProcessCommandException] {
        if ($_.FullyQualifiedErrorId -notlike 'NoProcessFoundForGivenId,*' -or -not (Is-Gone $Process)) { throw }
        return
    }
    try {
        # An open handle pins the PID, so the identity check and the stop below
        # act on the same process, never on a later one that reused the PID.
        try { $null = $Live.Handle } catch { if (Is-Gone $Process) { return }; throw }
        if (Is-Gone $Process) { return }
        Stop-Process -InputObject $Live -ErrorAction Stop
        if (-not $Live.WaitForExit(15000)) { throw "Susi process $($Process.ProcessId) did not exit within 15 s; nothing was replaced." }
    } finally { $Live.Dispose() }
}
$Owned = @(Owned-Processes)
if ($Action -eq 'busy') {
    $Owned | Where-Object { -not (Is-Service $_) } | ForEach-Object { $_.ProcessId }
    exit 0
}
if ($Action -in 'running','swap') {
    if ($Owned | Where-Object { Is-Daemon $_ }) { 'running' }
    if ($Action -eq 'running') { exit 0 }
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
# Daemons first, then the services they leave behind, in a fixed order.
foreach ($Process in @($Owned | Where-Object { Is-Daemon $_ }) + @($Owned | Where-Object { -not (Is-Daemon $_) })) {
    Stop-Owned $Process
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
