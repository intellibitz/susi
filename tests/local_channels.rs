//! Actual Windows local-channel acceptance, independent of release hosting.

#[cfg(windows)]
#[test]
fn windows_local_channel_completes_governed_file_goal() {
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            "scripts/test-local-channel.ps1",
            "-Binary",
            env!("CARGO_BIN_EXE_susi"),
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("PowerShell must run the native Windows acceptance");
    assert!(
        output.status.success(),
        "actual local-channel goal failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
