$ErrorActionPreference = 'Stop'
# PowerShell 7 callers can pass a module path without Windows PowerShell modules.
$env:PSModulePath = (Join-Path $PSHOME 'Modules') + ';' + $env:PSModulePath
$Scratch = Join-Path ([IO.Path]::GetTempPath()) ('susi-release-sync-test-' + [guid]::NewGuid().ToString('N'))
$Helper = Join-Path $PSScriptRoot 'susi-release-sync-windows.ps1'
$OwnedBin = Join-Path $Scratch 'owned\susi.exe'
$OtherBin = Join-Path $Scratch 'other\susi.exe'
$Candidate = Join-Path $Scratch 'candidate.exe'
$Ready = Join-Path $Scratch 'ready'
$Processes = @()
function Check($Condition, $Message) { if (-not $Condition) { throw $Message } }
# Waits, bounded, until a daemon fixture has put its leaf service in its job,
# and returns that leaf: a `service-run` copy of the daemon's own image.
function Wait-Leaf($Daemon) {
    $Marker = Join-Path $Ready "$($Daemon.Id).leaf"
    $Deadline = [DateTime]::UtcNow.AddSeconds(30)
    while (-not (Test-Path -LiteralPath $Marker)) {
        $Daemon.Refresh()
        Check (-not $Daemon.HasExited) "Daemon fixture $($Daemon.Id) exited with code $($Daemon.ExitCode) before its leaf joined the job"
        Check ([DateTime]::UtcNow -lt $Deadline) "Daemon fixture $($Daemon.Id) did not report its leaf within 30 s"
        Start-Sleep -Milliseconds 50
    }
    $LeafId = [int](Get-Content -LiteralPath $Marker -Raw)
    $Leaf = Get-CimInstance Win32_Process -Filter "ProcessId=$LeafId"
    Check ($Leaf -and $Leaf.ParentProcessId -eq $Daemon.Id -and $Leaf.CommandLine -match '\sservice-run$') "Leaf $LeafId is not the daemon fixture's service-run child"
    Check ($Leaf.ExecutablePath -eq $Daemon.Path) "Leaf $LeafId runs $($Leaf.ExecutablePath), not the daemon's image"
    Get-Process -Id $LeafId
}
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
    # The fixture stands in for susi.exe. As `daemon-start` it does what the
    # real daemon's supervisor does since sockfix2: it puts its leaf service in
    # a kill-on-close Job Object, so stopping the daemon takes the leaf down
    # with it. Every Win32 call is checked; a fixture that could not build the
    # job exits nonzero instead of silently leaving an ordinary child behind.
    Add-Type -OutputAssembly $Candidate -OutputType ConsoleApplication -TypeDefinition @'
using System;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
public class SusiPromotionFixture {
    [StructLayout(LayoutKind.Sequential)]
    struct BasicLimits {
        public long PerProcessUserTimeLimit; public long PerJobUserTimeLimit; public uint LimitFlags;
        public UIntPtr MinimumWorkingSetSize; public UIntPtr MaximumWorkingSetSize; public uint ActiveProcessLimit;
        public UIntPtr Affinity; public uint PriorityClass; public uint SchedulingClass;
    }
    [StructLayout(LayoutKind.Sequential)]
    struct IoCounters {
        public ulong ReadOperationCount; public ulong WriteOperationCount; public ulong OtherOperationCount;
        public ulong ReadTransferCount; public ulong WriteTransferCount; public ulong OtherTransferCount;
    }
    [StructLayout(LayoutKind.Sequential)]
    struct ExtendedLimits {
        public BasicLimits BasicLimitInformation; public IoCounters IoInfo;
        public UIntPtr ProcessMemoryLimit; public UIntPtr JobMemoryLimit; public UIntPtr PeakProcessMemoryUsed; public UIntPtr PeakJobMemoryUsed;
    }
    const int JobObjectExtendedLimitInformation = 9;
    const uint JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x2000;
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern IntPtr CreateJobObject(IntPtr attributes, string name);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool SetInformationJobObject(IntPtr job, int infoClass, ref ExtendedLimits info, uint length);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
    // Held for the life of the process: the job closes, and kills its leaf, only when this process ends.
    static IntPtr Job;
    public static int Main(string[] args) {
        if (args.Length > 0 && args[0] == "--version") {
            string home = Environment.GetEnvironmentVariable("SUSI_HOME");
            if (!string.IsNullOrEmpty(home)) File.WriteAllText(Path.Combine(home, "fixture-runtime-home.txt"), home);
            Console.WriteLine("susi 0.0.1");
            return 0;
        }
        if (args.Length > 0 && args[0] == "daemon-start") {
            // A non-inheritable handle, so only this process's end closes the job.
            Job = CreateJobObject(IntPtr.Zero, null);
            if (Job == IntPtr.Zero) return 3;
            ExtendedLimits limits = new ExtendedLimits();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if (!SetInformationJobObject(Job, JobObjectExtendedLimitInformation, ref limits, (uint)Marshal.SizeOf(typeof(ExtendedLimits)))) return 4;
            ProcessStartInfo leaf = new ProcessStartInfo(Process.GetCurrentProcess().MainModule.FileName, "service-run");
            leaf.UseShellExecute = false;
            leaf.CreateNoWindow = true;
            Process child = Process.Start(leaf);
            if (!AssignProcessToJobObject(Job, child.Handle)) { child.Kill(); return 5; }
            string ready = Environment.GetEnvironmentVariable("SUSI_FIXTURE_READY");
            if (!string.IsNullOrEmpty(ready)) File.WriteAllText(Path.Combine(ready, Process.GetCurrentProcess().Id + ".leaf"), child.Id.ToString());
        }
        System.Threading.Thread.Sleep(120000);
        return 0;
    }
}
'@
    Copy-Item $Candidate $OwnedBin
    Copy-Item $Candidate $OtherBin
    $OriginalHash = (Get-FileHash $OwnedBin).Hash
    # A changed image proves replacement, not just a successful no-op.
    [IO.File]::AppendAllText($Candidate, 'new-release')
    $NewHash = (Get-FileHash $Candidate).Hash
    New-Item -ItemType Directory -Force -Path $Ready | Out-Null
    $env:SUSI_FIXTURE_READY = $Ready
    $Other = Start-Process $OtherBin -ArgumentList 'daemon-start' -WindowStyle Hidden -PassThru
    $Daemon = Start-Process $OwnedBin -ArgumentList 'daemon-start' -WindowStyle Hidden -PassThru
    $Client = Start-Process $OwnedBin -ArgumentList 'mission daemon-start' -WindowStyle Hidden -PassThru
    $Processes += $Other,$Daemon,$Client
    $OtherLeaf = Wait-Leaf $Other
    $Processes += $OtherLeaf
    $Leaf = Wait-Leaf $Daemon
    $Processes += $Leaf
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
    Check ($Swap.Text -match 'running') 'Swap did not report the daemon to restart'
    Check ($Swap.Code -eq 0) "Promotion failed: $($Swap.Text)"
    Check ((Get-FileHash $OwnedBin).Hash -eq $NewHash) 'Canonical executable not replaced'
    Check ((Get-FileHash (Join-Path (Split-Path $OwnedBin) 'susi.prev.exe')).Hash -eq $OriginalHash) 'Previous executable not preserved'
    Check (-not (Test-Path (Join-Path (Split-Path $OwnedBin) 'susi-engine.exe'))) 'Legacy duplicate retained'
    $Daemon.Refresh(); $Other.Refresh(); $Leaf.Refresh(); $OtherLeaf.Refresh()
    Check $Daemon.HasExited 'Owned daemon not stopped before replacement'
    Check $Leaf.HasExited 'Owned leaf service not stopped before replacement'
    Check (-not $Other.HasExited) 'Other installation was stopped'
    Check (-not $OtherLeaf.HasExited) "Other installation's leaf service was stopped"
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
        # The sync script runs under Git for Windows' bash. Find it from git
        # itself: `bash.exe` on PATH can be the WSL launcher in System32.
        $GitExecPath = (& git --exec-path).Trim()
        Check ($LASTEXITCODE -eq 0 -and $GitExecPath) 'git --exec-path failed'
        $Shell = Join-Path (Split-Path (Split-Path (Split-Path $GitExecPath))) 'bin\bash.exe'
        Check (Test-Path -LiteralPath $Shell -PathType Leaf) "Git for Windows bash not found at $Shell"
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
    # Run the real release-record checks on Windows paths: the record must name
    # the tag and the commit it points at, and the reference checksum is
    # reported for a matching and a differing build, never enforced. Windows
    # paths contain backslashes, which make filename-mode sha256sum escape its
    # hash.
    $Updater = Get-Content (Join-Path $PSScriptRoot 'susi-release-sync.sh') -Raw
    $Functions = foreach ($Name in 'record_field', 'check_record', 'record_env', 'record_sha', 'host_asset', 'report_reference') {
        $Body = [regex]::Match($Updater, "(?s)$Name\(\) \{.*?\r?\n\}").Value
        Check ($Body.Length -gt 0) "Release record function $Name missing"
        $Body
    }
    $ChecksumScript = Join-Path $Scratch 'checksum.sh'
    $Harness = @'
set -euo pipefail
log() { printf '%s\n' "$*" >&2; }
die() { log "error: $*"; exit 1; }
RECORD="$TARGET/record.json"
mkdir -p "$TARGET"
cat >"$RECORD" <<EOF
{
  "tag": "v0.0.1",
  "commit": "1111111111111111111111111111111111111111",
  "build": {
    "profile": "dist",
    "features": "susi/local-engine",
    "env": ["CARGO_PROFILE_DIST_LTO=false", "CARGO_PROFILE_DIST_CODEGEN_UNITS=16"]
  },
  "assets": [
    { "file": "susi-windows-x86_64.exe", "target": "x86_64-pc-windows-msvc", "package": "susi", "bin": "susi", "sha256": "$SUSI_TEST_CHECKSUM" }
  ]
}
EOF
'@
    $Harness += "`n" + ($Functions -join "`n") + "`n" + @'
check_record v0.0.1 1111111111111111111111111111111111111111
[ "$(record_env)" = "CARGO_PROFILE_DIST_LTO=false CARGO_PROFILE_DIST_CODEGEN_UNITS=16" ] || { echo 'record env misread' >&2; exit 1; }
( check_record v0.0.1 2222222222222222222222222222222222222222 ) 2>/dev/null && { echo 'a tag that points elsewhere was accepted' >&2; exit 1; }
report_reference "$SUSI_CHECKSUM_FIXTURE" 2>&1 | grep -q 'matches the reference build' || { echo 'matching checksum not reported' >&2; exit 1; }
sed -i 's/"sha256": "[0-9a-f]*"/"sha256": "0000000000000000000000000000000000000000000000000000000000000000"/' "$RECORD"
report_reference "$SUSI_CHECKSUM_FIXTURE" 2>&1 | grep -q 'differs from the reference build' || { echo 'differing checksum not reported' >&2; exit 1; }
'@
    [IO.File]::WriteAllText($ChecksumScript, $Harness.Replace("`r`n", "`n"))
    $StartInfo.Arguments = '"' + $ChecksumScript + '"'
    $StartInfo.EnvironmentVariables['TARGET'] = (Join-Path $Scratch 'native-backslash-target')
    $StartInfo.EnvironmentVariables['SUSI_CHECKSUM_FIXTURE'] = $Candidate
    $StartInfo.EnvironmentVariables['SUSI_TEST_CHECKSUM'] = $NewHash.ToLower()
    $ChecksumProcess = [Diagnostics.Process]::Start($StartInfo)
    Check ($ChecksumProcess.WaitForExit(30000)) 'Release record integration timed out'
    $ChecksumOutput = $ChecksumProcess.StandardOutput.ReadToEnd() + $ChecksumProcess.StandardError.ReadToEnd()
    Check ($ChecksumProcess.ExitCode -eq 0) "Release record integration failed: $ChecksumOutput"
    'PASS: owned process safety, replacement, rollback, canonical home, release record checks and reference checksums on Windows paths'
} finally {
    foreach ($Process in $Processes) { if (-not $Process.HasExited) { Stop-Process -Id $Process.Id -ErrorAction SilentlyContinue } }
    # Images stay locked until their processes are gone; bound the wait.
    foreach ($Process in $Processes) { $Process.WaitForExit(10000) | Out-Null }
    Remove-Item Env:\SUSI_FIXTURE_READY -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $Scratch) { Remove-Item -LiteralPath $Scratch -Recurse -Force }
}
