//! `susi top` moved to Susi Commercial Apps (Mandate 84, VC-201-099): core
//! forwards it to `susi-commercial top`, and without that program installed it
//! names the command that serves it instead of failing silently.

use std::process::Command;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn susi_top_without_susi_commercial_names_the_command_that_serves_it() -> TestResult {
    let scratch = std::env::temp_dir().join(format!("susi-top-forward-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home)?;
    let mut command = Command::new(env!("CARGO_BIN_EXE_susi"));
    command.env_clear();
    for key in ["SystemRoot", "WINDIR", "PATHEXT"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    // No PATH entry and an empty home: no susi-commercial anywhere.
    for key in [
        "HOME",
        "USERPROFILE",
        "SUSI_HOME",
        "LOCALAPPDATA",
        "APPDATA",
    ] {
        command.env(key, &home);
    }
    let out = command
        .args(["top", "--json"])
        .env("PATH", &home)
        .env("SUSI_COMMERCIAL_BIN", home.join("absent"))
        .current_dir(&home)
        .output()?;
    let _ = std::fs::remove_dir_all(&scratch);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "susi top cannot answer without susi-commercial"
    );
    assert!(
        stderr.contains("susi-commercial top"),
        "the error names the command that serves it: {stderr}"
    );
    Ok(())
}
