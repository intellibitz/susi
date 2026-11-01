$ErrorActionPreference = 'Stop'
# PowerShell 7 callers can pass a module path without Windows PowerShell modules.
$env:PSModulePath = (Join-Path $PSHOME 'Modules') + ';' + $env:PSModulePath
$Scratch = Join-Path ([IO.Path]::GetTempPath()) ('susi-release-sync-test-' + [guid]::NewGuid().ToString('N'))
$Helper = Join-Path $PSScriptRoot 'susi-release-sync-windows.ps1'
$OwnedBin = Join-Path $Scratch 'owned\susi.exe'
$OtherBin = Join-Path $Scratch 'other\susi.exe'
$Candidate = Join-Path $Scratch 'candidate.exe'
$Processes = @()
function Check($Condition, $Message) { if (-not $Condition) { throw $Message } }
function Run-Helper($Action, $Built='') {
    $Arguments = @('-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',$Helper,'-Action',$Action,'-Bin',$OwnedBin)
    if ($Built) { $Arguments += @('-Built',$Built) }
    $ErrorActionPreference = 'Continue'
    $Output = & powershell.exe @Arguments 2>&1
    $ErrorActionPreference = 'Stop'
    [pscustomobject]@{ Code=$LASTEXITCODE; Text=($Output -join "`n") }
}
try {
    New-Item -ItemType Directory -Force -Path (Split-Path $OwnedBin),(Split-Path $OtherBin) | Out-Null
    Add-Type -TypeDefinition 'public class SusiPromotionFixture { public static void Main(string[] args) { if (args.Length > 0 && args[0] == "--version") { string home = System.Environment.GetEnvironmentVariable("SUSI_HOME"); if (!string.IsNullOrEmpty(home)) System.IO.File.WriteAllText(System.IO.Path.Combine(home, "fixture-runtime-home.txt"), home); System.Console.WriteLine("susi 0.0.1"); return; } System.Threading.Thread.Sleep(120000); } }' -OutputAssembly $Candidate -OutputType ConsoleApplication
    Copy-Item $Candidate $OwnedBin
    Copy-Item $Candidate $OtherBin
    $OriginalHash = (Get-FileHash $OwnedBin).Hash
    # A changed image proves replacement, not just a successful no-op.
    [IO.File]::AppendAllText($Candidate, 'new-release')
    $NewHash = (Get-FileHash $Candidate).Hash
    $Other = Start-Process $OtherBin -ArgumentList 'daemon-start' -WindowStyle Hidden -PassThru
    $Daemon = Start-Process $OwnedBin -ArgumentList 'daemon-start' -WindowStyle Hidden -PassThru
    $Client = Start-Process $OwnedBin -ArgumentList 'mission daemon-start' -WindowStyle Hidden -PassThru
    $Processes += $Other,$Daemon,$Client
    $Busy = Run-Helper 'busy'
    Check ($Busy.Code -eq 0 -and $Busy.Text -match "$($Client.Id)" -and $Busy.Text -notmatch "$($Other.Id)") 'Exact-owned client detection failed'
    Check ((Run-Helper 'running').Text -match 'running') 'Owned daemon not detected'
    $Refused = Run-Helper 'swap' $Candidate
    Check ($Refused.Code -ne 0) 'Active-client promotion must fail'
    Check ((Get-FileHash $OwnedBin).Hash -eq $OriginalHash) 'Refused promotion changed executable'
    Stop-Process -Id $Client.Id
    $Client.WaitForExit(10000) | Out-Null
    Copy-Item $OwnedBin (Join-Path (Split-Path $OwnedBin) 'susi-engine.exe')
    $Swap = Run-Helper 'swap' $Candidate
    Check ($Swap.Code -eq 0) "Promotion failed: $($Swap.Text)"
    Check ((Get-FileHash $OwnedBin).Hash -eq $NewHash) 'Canonical executable not replaced'
    Check ((Get-FileHash (Join-Path (Split-Path $OwnedBin) 'susi.prev.exe')).Hash -eq $OriginalHash) 'Previous executable not preserved'
    Check (-not (Test-Path (Join-Path (Split-Path $OwnedBin) 'susi-engine.exe'))) 'Legacy duplicate retained'
    $Daemon.Refresh(); $Other.Refresh()
    Check $Daemon.HasExited 'Owned daemon not stopped before replacement'
    Check (-not $Other.HasExited) 'Other installation was stopped'
    $Rollback = Run-Helper 'rollback'
    Check ($Rollback.Code -eq 0) "Rollback failed: $($Rollback.Text)"
    Check ((Get-FileHash $OwnedBin).Hash -eq $OriginalHash) 'Rollback did not restore executable'
    $Legacy = Join-Path (Split-Path $OwnedBin) 'susi-engine.exe'
    Copy-Item $OwnedBin $Legacy
    $LockedLegacy = [IO.File]::Open($Legacy, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        $CleanupFailure = Run-Helper 'swap' $Candidate
        Check ($CleanupFailure.Code -ne 0) 'Locked legacy cleanup should reject promotion'
        Check ((Get-FileHash $OwnedBin).Hash -eq $OriginalHash) 'Cleanup failure published new bytes before the marker'
    } finally { $LockedLegacy.Dispose() }
    # Exercise the real shell entrypoint and native environment propagation,
    # using a child process's isolated home, never the real installed Susi.
    $ShellHome = Join-Path $Scratch 'shell-home'
    $ShellInstall = Join-Path $ShellHome '.susi'
    $ShellBin = Join-Path $ShellInstall 'bin'
    New-Item -ItemType Directory -Path $ShellBin -Force | Out-Null
    Copy-Item $Candidate (Join-Path $ShellBin 'susi.exe')
    Copy-Item $OwnedBin (Join-Path $ShellBin 'susi.prev.exe')
        $Shell = (Get-Command bash.exe -ErrorAction Stop).Source
        $StartInfo = New-Object Diagnostics.ProcessStartInfo
        $StartInfo.FileName = $Shell
        $StartInfo.Arguments = '"' + (Join-Path $PSScriptRoot 'susi-release-sync.sh') + '" --rollback'
        $StartInfo.UseShellExecute = $false
        $StartInfo.CreateNoWindow = $true
        $StartInfo.RedirectStandardOutput = $true
        $StartInfo.RedirectStandardError = $true
        $StartInfo.EnvironmentVariables['HOME'] = $ShellHome
        $StartInfo.EnvironmentVariables['SUSI_HOME'] = (Join-Path $Scratch 'inherited-dev-home')
        $ShellProcess = [Diagnostics.Process]::Start($StartInfo)
        Check ($ShellProcess.WaitForExit(30000)) 'Shell rollback integration timed out'
        $ShellOutput = $ShellProcess.StandardOutput.ReadToEnd() + $ShellProcess.StandardError.ReadToEnd()
        Check ($ShellProcess.ExitCode -eq 0) "Shell rollback integration failed: $ShellOutput"
        Check ((Get-FileHash (Join-Path $ShellBin 'susi.exe')).Hash -eq $OriginalHash) 'Shell selected the wrong canonical executable'
        Check ((Get-Content (Join-Path $ShellInstall 'fixture-runtime-home.txt') -Raw) -eq $ShellInstall) 'Native executable received a different installation home'
    'PASS: owned client refusal, idle daemon replacement, unrelated process survival, canonical executable, legacy retirement, rollback'
} finally {
    foreach ($Process in $Processes) { if (-not $Process.HasExited) { Stop-Process -Id $Process.Id -ErrorAction SilentlyContinue } }
    if (Test-Path -LiteralPath $Scratch) { Remove-Item -LiteralPath $Scratch -Recurse -Force }
}
