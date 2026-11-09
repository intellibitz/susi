//! Deterministic Windows release identity race and fail-closed regressions.
#![cfg(windows)]
// Assertions report fixture failures directly.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[test]
fn cim_snapshot_identity_is_fail_closed() {
    let script = r#"
$ErrorActionPreference = 'Stop'
$tokens = $null; $errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($env:SUSI_IDENTITY_HELPER, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw 'Cannot parse production helper' }
foreach ($name in 'Owned-Processes','Is-Gone') {
    $node = $ast.Find({ param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq $name }, $true)
    if (-not $node) { throw "Missing production function $name" }
    Invoke-Expression $node.Extent.Text
}
$Bin = 'C:\fixture\owned\susi.exe'; $Legacy = 'C:\fixture\owned\susi-engine.exe'
$born = [datetime]'2026-01-01T00:00:00Z'
$other = [pscustomobject]@{ProcessId=7; CreationDate=$born; ExecutablePath='C:\fixture\other\susi.exe'}
$owned = [pscustomobject]@{ProcessId=8; CreationDate=$born; ExecutablePath=$Bin}
$unknown = [pscustomobject]@{ProcessId=9; CreationDate=$born; ExecutablePath=$null}
function Get-CimInstance {
    param($ClassName, $Filter)
    if ($ClassName -ne 'Win32_Process') { throw 'Unexpected class' }
    if ($Filter -like "Name=*") { return @($owned,$other,$unknown) }
    if ($Filter -ne 'ProcessId=9') { throw "Unexpected identity query $Filter" }
    if ($script:queryError) { throw 'CIM access denied' }
    $script:now
}
function Check-Owned {
    $actual = @(Owned-Processes)
    if ($actual.Count -ne 1 -or $actual[0].ProcessId -ne 8) { throw 'Wrong owned set' }
}
function Check-Refused {
    $refused = $false
    try { $null = @(Owned-Processes) } catch { $refused = $true }
    if (-not $refused) { throw 'Ambiguous/live process was accepted' }
}
# Exited row and confirmed PID reuse cannot block an unrelated installation.
$script:now = @(); Check-Owned
$script:now = @([pscustomobject]@{CreationDate=$born.AddSeconds(1)}); Check-Owned
# Still-live inaccessible process and missing birth times remain fail closed.
$script:now = @([pscustomobject]@{CreationDate=$born}); Check-Refused
$script:now = @([pscustomobject]@{CreationDate=$null}); Check-Refused
$unknown.CreationDate = $null
$script:now = @([pscustomobject]@{CreationDate=$born}); Check-Refused
$script:now = @([pscustomobject]@{CreationDate=$null}); Check-Refused
$script:queryError = $true; Check-Refused
'PASS: 7 identity cases'
"#;
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env(
            "SUSI_IDENTITY_HELPER",
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("scripts/susi-release-sync-windows.ps1"),
        )
        .output()
        .expect("run production identity function regressions");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("PASS: 7 identity cases"), "{stdout}");
}
