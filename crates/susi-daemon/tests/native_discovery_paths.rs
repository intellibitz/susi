#![cfg(any(unix, windows))]

use std::ffi::OsString;
use susi_daemon::discovery_pipeline::{Catalogs, prime_selected_catalogs};

fn native_home() -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(b"home-\xff".to_vec())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[104, 111, 109, 101, 45, 0xd800])
    }
}

#[test]
fn catalog_reports_preserve_native_pack_paths() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("SUSI_NATIVE_DISCOVERY_CHILD").is_some() {
        let home = susi_paths::SusiDirs::config_dir();
        assert_eq!(
            home.to_str().is_none(),
            std::env::var("SUSI_NATIVE_DISCOVERY_CHILD")? == "native"
        );
        let expected = susi_config::extensions::active_pack().root;
        for (catalogs, kind) in [
            (Catalogs::NONE, "auto_prime_partial"),
            (Catalogs::ALL, "auto_prime"),
        ] {
            let report = prime_selected_catalogs(&home, catalogs);
            assert_eq!(report["kind"], kind);
            assert_eq!(
                susi_paths::native_path::decode(&report["pack_root"])?,
                expected
            );
            assert_eq!(
                report["pack_root"],
                susi_paths::native_path::encode(&expected)?
            );
            if catalogs.is_all() {
                assert_eq!(report["mcp_enabled"], serde_json::json!([]));
                assert!(report.get("preferred_coding_model").is_some());
                assert!(report.get("agents_ready").is_some());
                assert!(report.get("frameworks_ready").is_some());
                assert!(report.get("primed").is_none());
                let persisted: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(home.join("last_auto_prime.json"))?)?;
                assert_eq!(persisted, report);
            } else {
                assert_eq!(report["primed"], serde_json::json!([]));
                assert!(report.get("preferred_coding_model").is_none());
                assert!(report.get("agents_ready").is_none());
                assert!(report.get("frameworks_ready").is_none());
                assert!(report.get("mcp_enabled").is_none());
                assert!(!home.join("last_auto_prime.json").exists());
            }
        }
        return Ok(());
    }
    let root = susi_paths::tempfile::tempdir()?;
    for (name, mode) in [(OsString::from("home"), "utf8"), (native_home(), "native")] {
        let home = root.path().join(name);
        std::fs::create_dir(&home)?;
        let mut child = std::process::Command::new(std::env::current_exe()?);
        child
            .args(["--exact", "catalog_reports_preserve_native_pack_paths"])
            .env_clear()
            .current_dir(root.path())
            .env("HOME", root.path())
            .env("USERPROFILE", root.path())
            .env("SUSI_HOME", &home)
            .env("SUSI_NO_REGISTRY_ENV", "1")
            .env("TMPDIR", root.path())
            .env("TEMP", root.path())
            .env("TMP", root.path())
            .env("PATH", root.path().join("no-tools"))
            .env("SUSI_NATIVE_DISCOVERY_CHILD", mode);
        if let Some(system) = std::env::var_os("SystemRoot") {
            child.env("SystemRoot", system);
        }
        let output = child.output()?;
        assert!(
            output.status.success(),
            "native discovery child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}
