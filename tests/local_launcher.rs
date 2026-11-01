//! Windows installation, connection reuse, and environment isolation checks.

#[cfg(windows)]
#[test]
fn windows_launcher_reuses_connections_without_touching_canonical_home(
) -> Result<(), Box<dyn std::error::Error>> {
    let scratch = std::env::temp_dir().join(format!(
        "susi-launcher-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&scratch)?;
    let check = scratch.join("check.ps1");
    std::fs::write(
        &check,
        r#"param([string]$Source,[string]$Binary,[string]$Root)
$ErrorActionPreference='Stop'
$canonical=Join-Path $Root 'canonical'
New-Item -ItemType Directory (Join-Path $canonical 'models') -Force | Out-Null
$cloud=Join-Path $canonical 'cloud.env'
[IO.File]::WriteAllText($cloud,"SUSI_LAUNCH_FIXTURE_API_KEY=fixture-only`nPATH=must-not-import`nSUSI_HOME=must-not-import`n")
$cloudHash=(Get-FileHash $cloud).Hash
$channels=Join-Path $Root 'channels'
$channel=Join-Path $channels 'dev'
New-Item -ItemType Directory $channel -Force | Out-Null
@{current=$Binary;sha256=(Get-FileHash $Binary).Hash.ToLowerInvariant()} | ConvertTo-Json | Set-Content (Join-Path $channel 'channel.json')
& $Source -Action Install -Channel dev -Root $channels -CanonicalHome $canonical -ShortcutDirectory (Join-Path $Root 'menu')
$installed=Join-Path $channel 'launcher.ps1'
$menu=Join-Path $Root 'menu/Susi (development).lnk'
if (!(Test-Path $installed) -or !(Test-Path $menu)) { throw 'Durable launcher or Start Menu app missing' }
$shell=New-Object -ComObject WScript.Shell
$shortcut=$shell.CreateShortcut($menu)
if (!$shortcut.Arguments.Contains($installed) -or $shortcut.Arguments.Contains($Source)) { throw 'Shortcut still depends on checkout' }
$oldPath=$env:PATH
$oldHome=$env:SUSI_HOME
$oldKey=$env:SUSI_LAUNCH_FIXTURE_API_KEY
& $installed -Channel dev -Root $channels -CanonicalHome $canonical -SusiArgs @('--version')
if ($env:PATH -ne $oldPath -or $env:SUSI_HOME -ne $oldHome -or $env:SUSI_LAUNCH_FIXTURE_API_KEY -ne $oldKey) { throw 'Launcher leaked environment changes' }
$config=Get-Content (Join-Path $channel 'home/config.json') -Raw | ConvertFrom-Json
if ((Join-Path $canonical 'models') -notin $config.local_scan_paths) { throw 'Existing models were not registered for read-only discovery' }
if ((Get-FileHash $cloud).Hash -ne $cloudHash -or (Get-ChildItem (Join-Path $canonical 'models')).Count) { throw 'Canonical connections or model cache changed' }
if (Test-Path (Join-Path $channel 'home/cloud.env')) { throw 'Provider secrets were copied' }
"#,
    )?;
    let output = std::process::Command::new("powershell.exe")
        // A Rust child does not get PowerShell 7's automatic removal of its
        // Core-only module paths when launching Windows PowerShell 5.1.
        .env_remove("PSModulePath")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&check)
        .arg("-Source")
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/susi-local-launch.ps1"))
        .args(["-Binary", env!("CARGO_BIN_EXE_susi"), "-Root"])
        .arg(&scratch)
        .output()?;
    assert!(
        output.status.success(),
        "installed launcher check failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_dir_all(&scratch)?;
    Ok(())
}
