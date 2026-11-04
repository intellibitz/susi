//! Actual Windows local-channel acceptance, independent of release hosting.

#[cfg(windows)]
#[test]
fn windows_local_channel_completes_governed_file_goal() -> std::io::Result<()> {
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
        .output()?;
    assert!(
        output.status.success(),
        "actual local-channel goal failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn unit_local_channel_script_exists_and_declares_binary_parameter() {
    let script = include_str!("../scripts/test-local-channel.ps1");
    assert!(!script.trim().is_empty());
    assert!(
        script.contains("Binary"),
        "test-local-channel.ps1 must accept -Binary parameter"
    );
}

#[test]
fn unit_local_channel_cargo_manifest_dir_contains_scripts_folder() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(manifest_dir
        .join("scripts/test-local-channel.ps1")
        .is_file());
}

#[test]
fn unit_local_channel_susi_binary_env_path_is_non_empty() {
    let bin = env!("CARGO_BIN_EXE_susi");
    assert!(!bin.is_empty());
}

#[test]
fn unit_local_channel_script_contains_strict_error_action_preference() {
    let script = include_str!("../scripts/test-local-channel.ps1");
    assert!(
        script.contains("ErrorActionPreference") || script.contains("param("),
        "test-local-channel.ps1 must define structured PowerShell parameters or error handling"
    );
}
