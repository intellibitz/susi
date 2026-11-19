#![cfg(unix)]

use std::ffi::OsString;

fn native(value: &str) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    let mut bytes = value.as_bytes().to_vec();
    bytes.insert(0, 0xff);
    OsString::from_vec(bytes)
}

#[test]
fn native_environment_credentials_are_masked() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("SUSI_NATIVE_REDACT_CHILD").is_some() {
        let value = native("fixture-native-credential");
        let text = format!(
            "native={} normal=fixture-normal-credential key=fixture-key-credential",
            value.to_string_lossy()
        );
        // Boolean assertion avoids printing credential values on failure.
        assert!(
            susi_error::redact::mask_env_credentials(&text)
                == "native=[REDACTED] normal=[REDACTED] key=[REDACTED]"
        );
        return Ok(());
    }
    let home = std::env::temp_dir().join(format!("susi-redact-env-{}", std::process::id()));
    std::fs::create_dir_all(&home)?;
    let mut child = std::process::Command::new(std::env::current_exe()?);
    child
        .args(["--exact", "native_environment_credentials_are_masked"])
        .env_clear()
        .env("SUSI_HOME", &home)
        .env("TMPDIR", &home)
        .env("TEMP", &home)
        .env("TMP", &home)
        .env("SUSI_NATIVE_REDACT_CHILD", "1")
        .env("GIT_CONFIG_GLOBAL", native("unrelated-path"))
        .env("FIXTURE_NATIVE_SECRET", native("fixture-native-credential"))
        .env("FIXTURE_API_KEY", "fixture-normal-credential")
        .env(native("FIXTURE_TOKEN"), "fixture-key-credential");
    susi_paths::test_env::isolate_home(&mut child, &home);
    if let Some(system) = std::env::var_os("SystemRoot") {
        child.env("SystemRoot", system);
    }
    let output = child.output()?;
    std::fs::remove_dir_all(home)?;
    assert!(output.status.success());
    Ok(())
}
