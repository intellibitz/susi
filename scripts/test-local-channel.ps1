[CmdletBinding()]
param([string]$Binary)
$ErrorActionPreference = 'Stop'
if (!$Binary) { $Binary = Join-Path (Split-Path $PSScriptRoot -Parent) 'target/debug/susi.exe' }
$Binary = (Resolve-Path -LiteralPath $Binary).Path
$scratch = Join-Path ([IO.Path]::GetTempPath()) ('susi-local-check-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $scratch | Out-Null
$nonce = 'local-proof-' + [Guid]::NewGuid().ToString('N')
[IO.File]::WriteAllText((Join-Path $scratch 'proof.txt'), $nonce)
$start = New-Object Diagnostics.ProcessStartInfo
$start.FileName = $Binary
$start.Arguments = 'shell'
$start.WorkingDirectory = $scratch
$start.UseShellExecute = $false
$start.CreateNoWindow = $true
$start.RedirectStandardInput = $true
$start.RedirectStandardOutput = $true
$start.RedirectStandardError = $true
# Every writable application root is isolated; cloud credentials are unnecessary
# for this actual governed file goal and must not escape into acceptance logs.
foreach ($key in @($start.EnvironmentVariables.Keys)) {
    if ($key -match '(_API_KEY|_API_BASE|_BASE_URL)$') { $start.EnvironmentVariables.Remove($key) }
}
foreach ($key in @('HOME','USERPROFILE','APPDATA','LOCALAPPDATA','TEMP','TMP')) { $start.EnvironmentVariables[$key] = $scratch }
$start.EnvironmentVariables['SUSI_HOME'] = Join-Path $scratch 'home'
New-Item -ItemType Directory -Path (Join-Path $scratch 'home') | Out-Null
[IO.File]::WriteAllText((Join-Path $scratch 'home/privacy_mode'), 'local_only')
$start.EnvironmentVariables['SUSI_PORT_OFFSET'] = '400'
$start.EnvironmentVariables['SUSI_OFFLINE'] = '1'
$start.EnvironmentVariables['RUST_LOG'] = 'warn'
$start.EnvironmentVariables.Remove('SUSI_VERBOSE')
$process = New-Object Diagnostics.Process
$process.StartInfo = $start
$daemonStopped = $false
function Stop-IsolatedDaemon {
    $stop = New-Object Diagnostics.Process
    $stop.StartInfo = $start
    $stop.StartInfo.Arguments = 'stop'
    try {
        if ($stop.Start()) {
            $stop.StandardInput.Close()
            $ignoreOut = $stop.StandardOutput.ReadToEndAsync()
            $ignoreErr = $stop.StandardError.ReadToEndAsync()
            if (!$stop.WaitForExit(10000)) { & taskkill /PID $stop.Id /T /F 2>$null | Out-Null }
        }
    } finally { $stop.Dispose() }
}
try {
    if (!$process.Start()) { throw 'Candidate could not start.' }
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    $goal = 'Read proof.txt and return its exact contents.'
    $process.StandardInput.WriteLine($goal)
    $process.StandardInput.WriteLine('exit')
    $process.StandardInput.Close()
    if (!$process.WaitForExit(60000)) {
        & taskkill /PID $process.Id /T /F 2>$null | Out-Null
        throw "Local first-goal verification exceeded 60 seconds. Evidence retained: $scratch"
    }
    Stop-IsolatedDaemon
    $daemonStopped = $true
    if (!$stdout.Wait(3000) -or !$stderr.Wait(3000)) { throw "Candidate left output handles open. Evidence retained: $scratch" }
    $out = $stdout.GetAwaiter().GetResult()
    $err = $stderr.GetAwaiter().GetResult()
    [IO.File]::WriteAllText((Join-Path $scratch 'stdout.txt'), $out)
    [IO.File]::WriteAllText((Join-Path $scratch 'stderr.txt'), $err)
    if ($process.ExitCode -ne 0 -or !$out.Contains($nonce)) { throw "Local first-goal verification failed. Evidence retained: $scratch" }
    $trace = Get-Content -LiteralPath (Join-Path $scratch '.susi/last_mission_trace.json') -Raw | ConvertFrom-Json
    if ($trace.goal -ne $goal -or $trace.status -notin @('SUCCESS','COMPLETE') -or !$trace.final_answer.Contains($nonce)) { throw "No successful governed result recorded. Evidence retained: $scratch" }
    Write-Output "Local installed candidate completed a fresh file goal. Evidence: $scratch"
} finally {
    if (!$daemonStopped) { Stop-IsolatedDaemon }
    $process.Dispose()
}
