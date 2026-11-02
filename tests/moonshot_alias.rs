//! Verify the runtime-loaded provider alias without creating a credential.
#![allow(clippy::unwrap_used)] // Failed setup must fail this integration test.

use std::{fs, path::Path, process::Command};

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
        cmd.env("SUSI_HOME", &home)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .args(args)
            .output()
            .unwrap()
    };
    assert!(run(&["extensions", "seed"]).status.success());
    fs::create_dir(&pack).unwrap();
    fs::write(pack.join("manifest.json"), r#"{"id":"moonshot-test","name":"Alias acceptance","version":"1.0.0","apiVersion":"1","capabilities":["catalog.cloud_vendors"],"permissions":[],"files":{"cloud-vendors.json":"cloud-vendors.json"}}"#).unwrap();
    let catalog = Path::new(option_env!("CARGO_MANIFEST_DIR").unwrap_or("."))
        .join("config/extensions/default/cloud-vendors.json");
    fs::copy(catalog, pack.join("cloud-vendors.json")).unwrap();
    let loaded = run(&["extensions", "load", "moonshot-test"]);
    assert!(loaded.status.success(), "{loaded:?}");
    let lookup = run(&["keys", "remove", "moonshotai"]);
    assert!(!lookup.status.success());
    assert!(
        String::from_utf8_lossy(&lookup.stderr).contains("MOONSHOT_API_KEY was not registered"),
        "{lookup:?}"
    );
    assert!(run(&["extensions", "unload", "moonshot-test"])
        .status
        .success());
}
