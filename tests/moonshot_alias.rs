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
