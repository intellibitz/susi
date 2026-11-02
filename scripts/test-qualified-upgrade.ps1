$ErrorActionPreference = 'Stop'
$env:PSModulePath = (Join-Path $PSHOME 'Modules') + ';' + $env:PSModulePath
$Scratch = [IO.Path]::GetFullPath((Join-Path ([IO.Path]::GetTempPath()) ('susi-qualified-test-' + [guid]::NewGuid().ToString('N'))))
$env:SUSI_HOME = $Scratch
$BinDir = Join-Path $Scratch 'bin'
[void][IO.Directory]::CreateDirectory($BinDir)
$Bin = Join-Path $BinDir 'susi.exe'
$Candidate = Join-Path $Scratch 'candidate.exe'
$Marker = Join-Path $BinDir 'susi.build.json'
$env:PATH = $BinDir + ';' + $env:PATH
$Port = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0)
$Port.Start()
$env:SUSI_PORT_OFFSET = [string]($Port.LocalEndpoint.Port - 9091)
$Port.Stop()
function Check($Condition,$Message) { if (!$Condition) { throw $Message } }
function Invoke-Upgrade($Digest) {
    $Script = Join-Path $Scratch 'upgrade-qualified-windows.ps1'
    & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $Script $Candidate $Digest ('a' * 40)
    $script:Code = $LASTEXITCODE
}
try {
    Copy-Item (Join-Path $PSScriptRoot 'upgrade-qualified-windows.ps1') $Scratch
    # Scope the fixture's process inventory to its private installation.
    # Exercise the real release helper, including Windows image locks and rollback,
    # without depending on unrelated Susi processes starting and exiting on the host.
    $env:SUSI_FIXTURE_HELPER = Join-Path $PSScriptRoot 'susi-release-sync-windows.ps1'
    [IO.File]::WriteAllText((Join-Path $Scratch 'susi-release-sync-windows.ps1'), @'
param($Action,$Bin,$Built)
function Get-CimInstance($ClassName,$Filter) {
    CimCmdlets\Get-CimInstance -ClassName $ClassName -Filter $Filter |
        Where-Object { $_.ExecutablePath -and [IO.Path]::GetDirectoryName($_.ExecutablePath) -eq [IO.Path]::GetDirectoryName($Bin) }
}
& $env:SUSI_FIXTURE_HELPER @PSBoundParameters
'@)
    Add-Type -OutputAssembly $Candidate -OutputType ConsoleApplication -TypeDefinition @'
using System;
using System.Net;
using System.Net.Sockets;
using System.Text;
public class Fixture {
    public static void Main(string[] args) {
        if (args.Length == 1 && args[0] == "--version") { Console.WriteLine("susi 1.2.3"); return; }
        if (args.Length == 2 && args[0] == "workflow") { System.Threading.Thread.Sleep(120000); return; }
        if (Environment.GetEnvironmentVariable("SUSI_FIXTURE_FAIL") == "1") {
            System.IO.File.WriteAllText(System.IO.Path.Combine(Environment.GetEnvironmentVariable("SUSI_HOME"), "failed-start"), "attempted");
            return;
        }
        var server = new TcpListener(IPAddress.Loopback, 9091 + int.Parse(Environment.GetEnvironmentVariable("SUSI_PORT_OFFSET")));
        server.Start();
        while (true) using (var client = server.AcceptTcpClient()) {
            var stream = client.GetStream();
            stream.Read(new byte[8192], 0, 8192);
            var response = Encoding.ASCII.GetBytes("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
            stream.Write(response, 0, response.Length);
        }
    }
}
'@
    Copy-Item $Candidate $Bin
    [IO.File]::AppendAllText($Candidate,'qualified image')
    $Original = (Get-FileHash $Bin).Hash
    $Digest = (Get-FileHash $Candidate).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText($Marker,'{"channel":"release","version":"1.0.0"}')
    [IO.File]::WriteAllText((Join-Path $Scratch 'config.json'),'{"port_offset":0}')
    $InitialMarker = (Get-FileHash $Marker).Hash
    Invoke-Upgrade ('0' * 64)
    Check ($Code -ne 0 -and (Get-FileHash $Bin).Hash -eq $Original) 'Digest mismatch must preserve original'
    $Watcher = Start-Process $Bin -ArgumentList workflow,watch -WindowStyle Hidden -PassThru
    try {
        Invoke-Upgrade $Digest
        Check ($Code -ne 0 -and !$Watcher.HasExited -and (Get-FileHash $Bin).Hash -eq $Original) 'Active watcher must be left running'
    } finally { if (!$Watcher.HasExited) { $Watcher.Kill(); $Watcher.WaitForExit() } }
    $env:SUSI_FIXTURE_FAIL = '1'
    for ($i = 0; $i -lt 3; $i++) {
        Invoke-Upgrade $Digest
        if (Test-Path (Join-Path $Scratch 'failed-start')) { break }
    }
    Check (Test-Path (Join-Path $Scratch 'failed-start')) 'Fixture must reach failed daemon startup'
    Check ($Code -ne 0 -and (Get-FileHash $Bin).Hash -eq $Original -and (Get-FileHash $Marker).Hash -eq $InitialMarker) 'Health failure did not restore original'
    Remove-Item Env:SUSI_FIXTURE_FAIL
    Invoke-Upgrade $Digest
    Check ($Code -eq 0 -and (Get-FileHash $Bin).Hash -eq $Digest) 'Upgrade failed'
    Check ((Get-FileHash (Join-Path $BinDir 'susi.prev.exe')).Hash -eq $Original) 'Rollback executable missing'
    Check ((Get-Content $Marker -Raw | ConvertFrom-Json).commit -eq ('a' * 40)) 'Release marker mismatch'
    Check ((Get-Content (Join-Path $Scratch 'config.json') -Raw) -eq '{"port_offset":0}') 'Configuration changed'
    Invoke-Upgrade $Digest
    Check ($Code -eq 0 -and (Get-FileHash (Join-Path $BinDir 'susi.prev.exe')).Hash -eq $Original) 'Retry lost original'
} finally {
    foreach ($Process in (Get-Process -Name susi -ErrorAction SilentlyContinue | Where-Object Path -eq $Bin)) {
        $Process.Kill()
        $Process.WaitForExit()
    }
    if ([IO.Path]::GetDirectoryName($Scratch) -ne [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')) { throw 'unexpected scratch location' }
    Remove-Item -LiteralPath $Scratch -Recurse -Force
}
