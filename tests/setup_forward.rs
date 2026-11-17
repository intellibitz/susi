//! `susi setup` moved to Susi Commercial Apps (Mandate 84, VC-201-069): core
//! forwards it to `susi-commercial setup`, and without that program installed
//! it names the command that serves it instead of failing silently.

use std::process::Command;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn susi_setup_without_susi_commercial_names_the_command_that_serves_it() -> TestResult {
    let scratch = std::env::temp_dir().join(format!("susi-setup-forward-{}", std::process::id()));
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
    susi_paths::test_env::isolate_home(&mut command, &home);
    command.env("SUSI_HOME", &home);
    let out = command
        .args(["setup", "plan", "--json"])
        .env("PATH", &home)
        .env("SUSI_COMMERCIAL_BIN", home.join("absent"))
        .current_dir(&home)
        .output()?;
    let _ = std::fs::remove_dir_all(&scratch);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "susi setup cannot answer without susi-commercial"
    );
    assert!(
        stderr.contains("susi-commercial setup"),
        "the error names the command that serves it: {stderr}"
    );
    Ok(())
}
