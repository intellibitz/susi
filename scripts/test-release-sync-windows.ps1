$ErrorActionPreference = 'Stop'
# PowerShell 7 callers can pass a module path without Windows PowerShell modules.
$env:PSModulePath = (Join-Path $PSHOME 'Modules') + ';' + $env:PSModulePath
$Scratch = Join-Path ([IO.Path]::GetTempPath()) ('susi-release-sync-test-' + [guid]::NewGuid().ToString('N'))
$Harness = Join-Path $Scratch 'harness'
$Helper = Join-Path $Harness 'susi-release-sync-windows.ps1'
$CatalogPath = Join-Path $Harness 'processes.xml'
$OwnedBin = Join-Path $Scratch 'owned\susi.exe'
$OtherBin = Join-Path $Scratch 'other\susi.exe'
$Candidate = Join-Path $Scratch 'candidate.exe'
$Ready = Join-Path $Scratch 'ready'
$Processes = @()
$Catalog = @()
function Check($Condition, $Message) { if (-not $Condition) { throw $Message } }
# Register only children this fixture started (or its verified job leaves).
# Cleanup admission is separate: a rejected discovered process must never be
# stopped by finally. Callers retain only direct children or verified job leaves.
function Register-Fixture($Process) {
    $null = $Process.Handle
    $Rows = @(CimCmdlets\Get-CimInstance Win32_Process -Filter "ProcessId=$($Process.Id)")
    Check ($Rows.Count -eq 1 -and $Rows[0].CreationDate -and $Rows[0].ExecutablePath -eq $Process.Path) 'Cannot register fixture process identity'
    Check ($Rows[0].ExecutablePath -eq $OwnedBin -or $Rows[0].ExecutablePath -eq $OtherBin) 'Fixture image is outside the two test installations'
    $script:Catalog += [pscustomobject]@{ ProcessId=$Process.Id; CreationDate=$Rows[0].CreationDate; ExecutablePath=$Rows[0].ExecutablePath }
}
# busy emits one complete decimal PID per line. Substring regex matching can
# confuse unrelated PIDs (for example, 1234 and 51234) and fail a safe swap.
function Has-PidLine([string]$Text, [int]$ProcessId) {
    @($Text -split '\r?\n') -contains [string]$ProcessId
}
Check (Has-PidLine "51234`r`n67890" 51234) 'CRLF PID line not recognized'
Check (Has-PidLine "51234`n67890" 67890) 'LF PID line not recognized'
Check (-not (Has-PidLine '51234' 1234)) 'PID suffix matched an unrelated process'
Check (-not (Has-PidLine '51234' 5123)) 'PID prefix matched an unrelated process'
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
    $CandidateProcess = Get-Process -Id $LeafId
    try {
        # Pin BEFORE querying identity. If the marker's PID was reused, the
        # new process must fail validation and is only disposed, never stopped.
        # Keeping this handle open prevents reuse after validation as well.
        $null = $CandidateProcess.Handle
        $Rows = @(CimCmdlets\Get-CimInstance Win32_Process -Filter "ProcessId=$LeafId")
        Check ($Rows.Count -eq 1 -and $Rows[0].CreationDate -and $Rows[0].ParentProcessId -eq $Daemon.Id -and $Rows[0].CommandLine -match '\sservice-run$') "Leaf $LeafId is not the daemon fixture's service-run child"
        Check ($Rows[0].ExecutablePath -eq $Daemon.Path) "Leaf $LeafId does not run the daemon's image"
        $CandidateProcess
    } catch {
        $CandidateProcess.Dispose()
        throw
    }
}
function Run-Helper($Action, $Built='', [int]$UnreadableProcessId=0, [switch]$ProbeOutsideCatalog) {
    $Catalog | Export-Clixml -LiteralPath $CatalogPath
    $Arguments = @('-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',$Helper,'-Action',$Action,'-Bin',$OwnedBin)
    if ($Built) { $Arguments += @('-Built',$Built) }
    if ($UnreadableProcessId) { $Arguments += @('-UnreadableProcessId',$UnreadableProcessId) }
    if ($ProbeOutsideCatalog) { $Arguments += '-ProbeOutsideCatalog' }
    $ErrorActionPreference = 'Continue'
    $Output = & powershell.exe @Arguments 2>&1
    $ErrorActionPreference = 'Stop'
    [pscustomobject]@{ Code=$LASTEXITCODE; Text=($Output -join "`n") }
}
try {
    New-Item -ItemType Directory -Force -Path (Split-Path $OwnedBin),(Split-Path $OtherBin) | Out-Null
    New-Item -ItemType Directory -Path $Harness | Out-Null
    # Run unchanged production code against a fixture-local process catalog.
    # No production test switch, global process discovery, or fake stop/wait.
    foreach ($Name in 'susi-release-sync.sh','posix-compat.sh','susi-release-sync-windows.ps1') {
        $Destination = if ($Name -eq 'susi-release-sync-windows.ps1') { Join-Path $Harness 'production-helper.ps1' } else { Join-Path $Harness $Name }
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot $Name) -Destination $Destination
        Check ((Get-FileHash $Destination).Hash -eq (Get-FileHash (Join-Path $PSScriptRoot $Name)).Hash) "Production source copy changed: $Name"
    }
    $Wrapper = @'
param([string]$Action, [string]$Bin, [string]$Built, [int]$UnreadableProcessId=0, [switch]$ProbeOutsideCatalog)
$ErrorActionPreference = 'Stop'
$env:PSModulePath = (Join-Path $PSHOME 'Modules') + ';' + $env:PSModulePath
$FixtureCatalog = @(Import-Clixml -LiteralPath (Join-Path $PSScriptRoot 'processes.xml'))
function Fixture-Entry([int]$ProcessId) {
    $Entry = @($FixtureCatalog | Where-Object { $_.ProcessId -eq $ProcessId })
    if ($Entry.Count -ne 1) { throw "Unregistered fixture PID: $ProcessId" }
    $Entry[0]
}
function Fixture-Row([int]$ProcessId) {
    $Entry = Fixture-Entry $ProcessId
    $Row = @(CimCmdlets\Get-CimInstance Win32_Process -Filter "ProcessId=$ProcessId")
    if ($Row.Count -eq 0) { return }
    if ($Row.Count -ne 1 -or $Row[0].CreationDate -ne $Entry.CreationDate -or $Row[0].ExecutablePath -ne $Entry.ExecutablePath) {
        throw "Fixture identity changed: $ProcessId"
    }
    $Row[0]
}
function Get-CimInstance {
    param($ClassName, $Filter)
    if ($ClassName -ne 'Win32_Process') { throw 'Unexpected fixture CIM class' }
    if ($Filter -eq "Name='susi.exe' OR Name='susi-engine.exe'") {
        foreach ($Entry in $FixtureCatalog) {
            $Row = Fixture-Row $Entry.ProcessId
            if (-not $Row) { continue }
            if ($Row.ProcessId -eq $UnreadableProcessId) {
                # Only this explicit negative case changes real process metadata.
                [pscustomobject]@{ ProcessId=$Row.ProcessId; CreationDate=$Row.CreationDate; ExecutablePath=$null; CommandLine=$Row.CommandLine }
            } else { $Row }
        }
    } elseif ($Filter -cmatch '^ProcessId=([0-9]+)$') {
        Fixture-Row ([int]$Matches[1])
    } else { throw "Unexpected fixture CIM filter: $Filter" }
}
function Get-Process {
    [CmdletBinding()] param([int]$Id)
    $null = Fixture-Entry $Id
    Microsoft.PowerShell.Management\Get-Process -Id $Id -ErrorAction Stop
}
function Stop-Process {
    [CmdletBinding()] param([Diagnostics.Process]$InputObject)
    $null = Fixture-Entry $InputObject.Id
    $null = $InputObject.Handle
    $null = Fixture-Row $InputObject.Id
    Microsoft.PowerShell.Management\Stop-Process -InputObject $InputObject -ErrorAction Stop
}
if ($ProbeOutsideCatalog) {
    # This helper process is known to the test but is deliberately not a
    # registered Susi child. A broken stop check can only kill this probe.
    $Self = [Diagnostics.Process]::GetCurrentProcess()
    try {
        foreach ($Operation in @(
            { Get-CimInstance Win32_Process -Filter "ProcessId=$($Self.Id)" },
            { Get-Process -Id $Self.Id },
            { Stop-Process -InputObject $Self }
        )) {
            $Refused = $false
            try { $null = & $Operation } catch {
                if ($_.Exception.Message -ne "Unregistered fixture PID: $($Self.Id)") { throw }
                $Refused = $true
            }
            if (-not $Refused) { throw 'Out-of-catalog process access was accepted' }
        }
    } finally { $Self.Dispose() }
    'PASS: fixture catalog rejects outside PIDs'
    exit 0
}
$Arguments = @{ Action=$Action; Bin=$Bin }
if ($Built) { $Arguments.Built = $Built }
& (Join-Path $PSScriptRoot 'production-helper.ps1') @Arguments
'@
    [IO.File]::WriteAllText($Helper, $Wrapper.Replace("`r`n", "`n"))
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
    $Processes += $Other
    Register-Fixture $Other
    $Daemon = Start-Process $OwnedBin -ArgumentList 'daemon-start' -WindowStyle Hidden -PassThru
    $Processes += $Daemon
    Register-Fixture $Daemon
    $Client = Start-Process $OwnedBin -ArgumentList 'mission daemon-start' -WindowStyle Hidden -PassThru
    $Processes += $Client
    Register-Fixture $Client
    $OtherClient = Start-Process $OtherBin -ArgumentList 'mission daemon-start' -WindowStyle Hidden -PassThru
    $Processes += $OtherClient
    Register-Fixture $OtherClient
    $OtherLeaf = Wait-Leaf $Other
    $Processes += $OtherLeaf
    Register-Fixture $OtherLeaf
    $Leaf = Wait-Leaf $Daemon
    $Processes += $Leaf
    Register-Fixture $Leaf
    # A stale/untrusted leaf marker must not admit its target into cleanup.
    # Use our real client as the wrong-parent target, never an unrelated PID.
    $LeafMarker = Join-Path $Ready "$($Daemon.Id).leaf"
    $RealLeafId = Get-Content -LiteralPath $LeafMarker -Raw
    $ProcessCount = $Processes.Count; $CatalogCount = $Catalog.Count
    try {
        [IO.File]::WriteAllText($LeafMarker, [string]$Client.Id)
        $Rejected = $false
        try { $null = Wait-Leaf $Daemon } catch {
            Check ($_.Exception.Message -eq "Leaf $($Client.Id) is not the daemon fixture's service-run child") 'Unexpected leaf rejection failure'
            $Rejected = $true
        }
        Check $Rejected 'Wrong-parent leaf marker was accepted'
        Check ($Processes.Count -eq $ProcessCount -and $Catalog.Count -eq $CatalogCount) 'Rejected leaf changed cleanup or process catalog'
        $Client.Refresh(); Check (-not $Client.HasExited) 'Rejected leaf marker stopped its target'
    } finally { [IO.File]::WriteAllText($LeafMarker, $RealLeafId) }
    'PASS: leaf admission rejects mismatched identity'
    $Outside = Run-Helper 'busy' -ProbeOutsideCatalog
    Check ($Outside.Code -eq 0 -and $Outside.Text -match 'PASS: fixture catalog rejects outside PIDs') "Catalog confinement failed: $($Outside.Text)"
    'PASS: fixture catalog rejects outside PIDs'
    # An unreadable LIVE other-installation image must refuse before filtering
    # by installation path, stopping anything, or staging replacement bytes.
    $Unknown = Run-Helper 'swap' $Candidate $Other.Id
    Check ($Unknown.Code -ne 0 -and $Unknown.Text -match 'Cannot establish executable ownership for a running Susi process') "Unknown live ownership was not refused: $($Unknown.Text)"
    Check ((Get-FileHash $OwnedBin).Hash -eq $OriginalHash) 'Unknown ownership changed installed bytes'
    Check (-not (Test-Path "$OwnedBin.new") -and -not (Test-Path (Join-Path (Split-Path $OwnedBin) 'susi.prev.exe'))) 'Unknown ownership staged or preserved release bytes'
    foreach ($Process in $Processes) { $Process.Refresh(); Check (-not $Process.HasExited) 'Unknown ownership stopped a fixture process' }
    'PASS: unreadable live ownership fails closed'
    $Busy = Run-Helper 'busy'
    Check ($Busy.Code -eq 0 -and (Has-PidLine $Busy.Text $Client.Id) -and -not (Has-PidLine $Busy.Text $Other.Id) -and -not (Has-PidLine $Busy.Text $OtherClient.Id)) "Exact-owned client detection failed: code=$($Busy.Code), client=$($Client.Id), other=$($Other.Id), output=[$($Busy.Text)]"
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
    $OtherClient.Refresh()
    Check (-not $OtherClient.HasExited) "Other installation's active client was stopped"
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
        $StartInfo.Arguments = '"' + (Join-Path $Harness 'susi-release-sync.sh') + '" --rollback'
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
    foreach ($Process in $Processes) { if (-not $Process.HasExited) { Stop-Process -InputObject $Process -ErrorAction SilentlyContinue } }
    # Images stay locked until their processes are gone; bound the wait.
    foreach ($Process in $Processes) { $Process.WaitForExit(10000) | Out-Null; $Process.Dispose() }
    Remove-Item Env:\SUSI_FIXTURE_READY -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $Scratch) { Remove-Item -LiteralPath $Scratch -Recurse -Force }
}
