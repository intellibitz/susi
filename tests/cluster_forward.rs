//! `susi peers` and `susi commits` moved to Susi Commercial Apps (Mandate 84,
//! Fleet Command): core forwards them to `susi-commercial peers|commits`, and
//! without that program installed it names the command that serves each
//! instead of failing silently.

use std::process::Command;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Run `susi <args>` with no `susi-commercial` anywhere; returns (success, stderr).
fn without_commercial(
    tag: &str,
    args: &[&str],
) -> Result<(bool, String), Box<dyn std::error::Error>> {
    let scratch =
        std::env::temp_dir().join(format!("susi-cluster-forward-{tag}-{}", std::process::id()));
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
        .args(args)
        .env("PATH", &home)
        .env("SUSI_COMMERCIAL_BIN", home.join("absent"))
        .current_dir(&home)
        .output()?;
    let _ = std::fs::remove_dir_all(&scratch);
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

#[test]
fn susi_peers_without_susi_commercial_names_the_command_that_serves_it() -> TestResult {
    let (ok, stderr) = without_commercial("peers", &["peers", "list", "--json"])?;
    assert!(!ok, "susi peers cannot answer without susi-commercial");
    assert!(
        stderr.contains("susi-commercial peers"),
        "the error names the command that serves it: {stderr}"
    );
    Ok(())
}

#[test]
fn susi_commits_without_susi_commercial_names_the_command_that_serves_it() -> TestResult {
    let (ok, stderr) = without_commercial("commits", &["commits", "audit", "--strict"])?;
    assert!(!ok, "susi commits cannot answer without susi-commercial");
    assert!(
        stderr.contains("susi-commercial commits"),
        "the error names the command that serves it: {stderr}"
    );
    Ok(())
}
