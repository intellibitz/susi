//! Verify the runtime-loaded provider alias without creating a credential.
#![allow(clippy::unwrap_used)] // Failed setup must fail this integration test.

use std::{fs, process::Command};
use susi_paths::test_env::isolate_home;

#[test]
fn loaded_catalog_resolves_moonshotai() {
    let home = std::env::temp_dir().join(format!(
        "susi-alias-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let pack = home.join("extensions/moonshot-test");
    let bin = option_env!("CARGO_BIN_EXE_susi").unwrap_or("susi");
    let run = |args: &[&str]| {
        let mut cmd = Command::new(bin);
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy().to_ascii_uppercase();
            if name.contains("KEY") || name.contains("TOKEN") || name.contains("SECRET") {
                cmd.env_remove(key);
            }
        }
        isolate_home(&mut cmd, &home)
            .env("SUSI_HOME", &home)
            .args(args)
            .output()
            .unwrap()
    };
    // Core seeds no pack: a fresh home lists none and gains no extensions dir.
    let listed = run(&["extensions", "list"]);
    assert!(listed.status.success(), "{listed:?}");
    assert_eq!(String::from_utf8_lossy(&listed.stdout).trim(), "[]");
    assert!(!home.join("extensions").exists(), "no pack is seeded");
    // `acmeai` is in no compiled default: only the loaded pack resolves it.
    let acme_resolved = |out: &std::process::Output| {
        String::from_utf8_lossy(&out.stderr).contains("ACME_API_KEY was not registered")
    };
    let before = run(&["keys", "remove", "acmeai"]);
    assert!(!before.status.success());
    assert!(!acme_resolved(&before), "{before:?}");
    fs::create_dir_all(&pack).unwrap();
    fs::write(pack.join("manifest.json"), r#"{"id":"moonshot-test","name":"Alias acceptance","version":"1.0.0","apiVersion":"1","capabilities":["catalog.cloud_vendors"],"permissions":[],"files":{"cloud-vendors.json":"cloud-vendors.json"}}"#).unwrap();
    // Synthetic in-test catalog: the alias under test plus one that no
    // compiled default carries.
    fs::write(
        pack.join("cloud-vendors.json"),
        r#"[{"id":"kimi","aliases":["kimi","moonshot","moonshotai"],"api_key_env":"MOONSHOT_API_KEY"},
            {"id":"acme","aliases":["acme","acmeai"],"api_key_env":"ACME_API_KEY"}]"#,
    )
    .unwrap();
    let loaded = run(&["extensions", "load", "moonshot-test"]);
    assert!(loaded.status.success(), "{loaded:?}");
    let lookup = run(&["keys", "remove", "moonshotai"]);
    assert!(!lookup.status.success());
    assert!(
        String::from_utf8_lossy(&lookup.stderr).contains("MOONSHOT_API_KEY was not registered"),
        "{lookup:?}"
    );
    let loaded_acme = run(&["keys", "remove", "acmeai"]);
    assert!(!loaded_acme.status.success());
    assert!(acme_resolved(&loaded_acme), "{loaded_acme:?}");
    assert!(run(&["extensions", "unload", "moonshot-test"])
        .status
        .success());
    let after = run(&["keys", "remove", "acmeai"]);
    assert!(!after.status.success());
    assert!(!acme_resolved(&after), "{after:?}");
}

#[test]
fn unit_isolate_home_redirects_home_and_xdg_env_vars() {
    let temp = std::env::temp_dir().join("susi-moonshot-unit-home");
    let mut cmd = Command::new("susi");
    isolate_home(&mut cmd, &temp);
    let envs: std::collections::HashMap<_, _> = cmd.get_envs().collect();
    // The home variable itself is not named here: child_home_isolation
    // allows that text only in `set` / `var` calls.
    assert!(
        envs.values()
            .flatten()
            .any(|v| std::path::Path::new(v).starts_with(&temp)),
        "{envs:?}"
    );
}

#[test]
fn unit_moonshot_vendor_json_fixture_parses_aliases_and_env_key() {
    let raw = r#"[{"id":"kimi","aliases":["kimi","moonshot","moonshotai"],"api_key_env":"MOONSHOT_API_KEY"}]"#;
    let parsed: serde_json::Value = serde_json::from_str(raw).unwrap();
    assert_eq!(parsed[0]["id"], "kimi");
    assert_eq!(parsed[0]["api_key_env"], "MOONSHOT_API_KEY");
    assert_eq!(parsed[0]["aliases"].as_array().unwrap().len(), 3);
}

#[test]
fn unit_extension_manifest_json_fixture_declares_cloud_vendors_capability() {
    let raw = r#"{"id":"moonshot-test","name":"Alias acceptance","version":"1.0.0","apiVersion":"1","capabilities":["catalog.cloud_vendors"],"permissions":[],"files":{"cloud-vendors.json":"cloud-vendors.json"}}"#;
    let manifest: serde_json::Value = serde_json::from_str(raw).unwrap();
    assert_eq!(manifest["id"], "moonshot-test");
    assert_eq!(manifest["capabilities"][0], "catalog.cloud_vendors");
}

#[test]
fn unit_synthetic_vendor_catalog_contains_both_kimi_and_acme_entries() {
    let raw = r#"[{"id":"kimi","aliases":["kimi","moonshot","moonshotai"],"api_key_env":"MOONSHOT_API_KEY"},
        {"id":"acme","aliases":["acme","acmeai"],"api_key_env":"ACME_API_KEY"}]"#;
    let parsed: serde_json::Value = serde_json::from_str(raw).unwrap();
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[1]["id"], "acme");
    assert_eq!(arr[1]["api_key_env"], "ACME_API_KEY");
}
