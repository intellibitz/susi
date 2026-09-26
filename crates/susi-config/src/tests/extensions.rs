//! Tests for `extensions` — kept out of the canonical source so crates that
//! `#[path]`-mount it never compile or run them.

use crate::extensions::*;

fn with_temp_home<F: FnOnce()>(f: F) {
    let _guard = crate::env_test_lock();
    let tmp = std::env::temp_dir().join(format!(
        "susi_ext_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    // Pre-create the legacy base so `crate::susi_paths::SusiDirs::use_xdg()` cannot flip
    // mid-test if a concurrent test creates it under the swapped HOME.
    let _ = std::fs::create_dir_all(tmp.join(".susi"));
    let prev_home = std::env::var_os("HOME");
    let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
    let prev_pack = std::env::var_os("SUSI_EXTENSION_PACK");
    let prev_susi_xdg = std::env::var_os("SUSI_XDG");
    unsafe {
        std::env::set_var("HOME", &tmp);
        std::env::set_var("XDG_CONFIG_HOME", tmp.join("config"));
        std::env::set_var("SUSI_XDG", "0");
        std::env::remove_var("SUSI_EXTENSION_PACK");
    }
    invalidate_extension_caches();
    f();
    unsafe {
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        match prev_xdg {
            Some(h) => std::env::set_var("XDG_CONFIG_HOME", h),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match prev_pack {
            Some(h) => std::env::set_var("SUSI_EXTENSION_PACK", h),
            None => std::env::remove_var("SUSI_EXTENSION_PACK"),
        }
        match prev_susi_xdg {
            Some(h) => std::env::set_var("SUSI_XDG", h),
            None => std::env::remove_var("SUSI_XDG"),
        }
    }
    invalidate_extension_caches();
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn bundled_manifest_is_default_pack() {
    let m = bundled_manifest();
    assert_eq!(m.id, "default");
    assert!(m.files.contains_key("cloud-vendors.json"));
    assert!(m.files.contains_key("coding-models.json"));
    assert!(validate_manifest(&m).is_ok());
    assert_eq!(m.api_version.chars().next(), Some('1'));
}

#[test]
fn validate_manifest_rejects_incompatible_api_major() {
    let mut m = bundled_manifest();
    m.api_version = "2.0.0".into();
    assert!(validate_manifest(&m).is_err());
}

#[test]
fn load_pack_rejects_incompatible_api_without_activating() {
    with_temp_home(|| {
        ensure_extensions_substrate().unwrap();
        let bad = extensions_root().join("badapi");
        private_dir(&bad).unwrap();
        write_private_file(
            &bad.join("manifest.json"),
            r#"{
  "id": "badapi",
  "name": "Bad",
  "version": "0.0.1",
  "apiVersion": "99",
  "files": {}
}"#,
        )
        .unwrap();
        let err = load_pack("badapi").expect_err("must reject");
        assert!(err.contains("incompatible") || err.contains("rejected"));
        assert_eq!(active_pack().id, "default");
    });
}

#[test]
fn bundled_cloud_vendors_parse() {
    let vendors: Vec<CloudVendorEntry> =
        serde_json::from_str(BUNDLED_CLOUD_VENDORS).expect("parse");
    assert!(vendors.len() >= 10);
    let openai = vendors.iter().find(|v| v.id == "openai").expect("openai");
    assert_eq!(openai.api_key_env, "OPENAI_API_KEY");
}

#[test]
fn load_json_or_bundled_uses_bundled_when_no_host_file() {
    // Must hold env_test_lock via with_temp_home: load_json_or_bundled
    // reaches ensure_extensions_substrate()/state.json, whose path derives
    // from HOME — running it while another test has HOME swapped would
    // race a lost update on that test's temp-root state.json.
    with_temp_home(|| {
        let vendors: Vec<CloudVendorEntry> =
            load_json_or_bundled("cloud-vendors.json", BUNDLED_CLOUD_VENDORS);
        assert!(!vendors.is_empty());
    });
}

#[test]
fn ensure_seeds_default_pack_and_state() {
    with_temp_home(|| {
        let pack = ensure_extensions_substrate().expect("seed");
        assert_eq!(pack.id, "default");
        assert!(pack.root.join("manifest.json").is_file());
        assert!(pack.root.join("cloud-vendors.json").is_file());
        assert!(pack.root.join("coding-models.json").is_file());
        assert!(state_path().is_file());
        let listed = list_packs();
        assert!(listed
            .iter()
            .any(|p| p.id == "default" && p.active && p.loaded));
    });
}

#[test]
fn load_unload_round_trip_custom_pack() {
    with_temp_home(|| {
        ensure_extensions_substrate().unwrap();
        let custom = extensions_root().join("custom");
        private_dir(&custom).unwrap();
        write_private_file(
            &custom.join("manifest.json"),
            r#"{
  "id": "custom",
  "name": "Custom",
  "version": "0.0.1",
  "files": { "cloud-vendors.json": "cloud-vendors.json" }
}"#,
        )
        .unwrap();
        write_private_file(
            &custom.join("cloud-vendors.json"),
            r#"[{"id":"acme","aliases":["acme"],"api_key_env":"ACME_API_KEY"}]"#,
        )
        .unwrap();

        let loaded = load_pack("custom").unwrap();
        assert!(loaded.active && loaded.loaded);
        assert_eq!(active_pack().id, "custom");

        let vendors = load_cloud_vendors();
        assert_eq!(vendors.len(), 1);
        assert_eq!(vendors[0].id, "acme");

        unload_pack("custom").unwrap();
        assert_eq!(active_pack().id, "default");
        let after = list_packs();
        assert!(after
            .iter()
            .any(|p| p.id == "custom" && !p.loaded && !p.active));
    });
}

#[test]
fn create_pack_writes_manifest_shell() {
    with_temp_home(|| {
        let status = create_pack("mine").unwrap();
        assert!(status.seeded);
        assert!(!status.active);
        assert!(extensions_root()
            .join("mine")
            .join("manifest.json")
            .is_file());
        let loaded = load_pack("mine").unwrap();
        assert!(loaded.active);
        unload_pack("mine").unwrap();
        assert_eq!(active_pack().id, "default");
    });
}

#[test]
fn validate_manifest_rejects_unknown_permission() {
    let mut m = bundled_manifest();
    m.permissions = vec!["kernel.root".into()];
    let err = validate_manifest(&m).expect_err("unknown permission must fail");
    assert!(err.contains("unknown permission"), "{err}");
}

#[test]
fn validate_manifest_rejects_escaping_files_without_permission() {
    let mut m = bundled_manifest();
    m.permissions = Vec::new();
    m.files
        .insert("evil.json".into(), "../../outside.json".into());
    let err = validate_manifest(&m).expect_err("escape without filesystem.read must fail");
    assert!(err.contains("filesystem.read"), "{err}");
}

#[test]
fn validate_manifest_allows_escaping_files_with_filesystem_read() {
    // The bundled default pack maps catalogs via `../../` and declares
    // filesystem.read — it must keep validating.
    let m = bundled_manifest();
    assert!(
        m.files.values().any(|v| v.starts_with("../")),
        "bundled manifest uses parent-relative files"
    );
    assert!(validate_manifest(&m).is_ok());
}

#[test]
fn validate_manifest_rejects_absolute_file_paths_even_with_permission() {
    let mut m = bundled_manifest();
    m.files.insert("abs.json".into(), "/etc/passwd".into());
    let err = validate_manifest(&m).expect_err("absolute path must fail");
    assert!(err.contains("absolute"), "{err}");
}

#[test]
fn load_pack_rejects_escaping_files_manifest() {
    with_temp_home(|| {
        ensure_extensions_substrate().unwrap();
        let bad = extensions_root().join("escape");
        private_dir(&bad).unwrap();
        write_private_file(
            &bad.join("manifest.json"),
            r#"{
  "id": "escape",
  "name": "Escape",
  "version": "0.0.1",
  "files": { "loot.json": "../../loot.json" }
}"#,
        )
        .unwrap();
        let err = load_pack("escape").expect_err("must reject");
        assert!(err.contains("filesystem.read"), "{err}");
        assert_eq!(active_pack().id, "default");
    });
}

#[test]
fn resolve_pack_path_jails_escapes_without_filesystem_read() {
    with_temp_home(|| {
        // Defense-in-depth: bypass load-time validation by writing the
        // manifest directly, then prove resolve_pack_path still refuses.
        ensure_extensions_substrate().unwrap();
        let dir = extensions_root().join("jailbreak");
        private_dir(&dir).unwrap();
        write_private_file(
            &dir.join("manifest.json"),
            r#"{
  "id": "jailbreak",
  "name": "Jailbreak",
  "version": "0.0.1",
  "files": { "loot": "../../loot.json" }
}"#,
        )
        .unwrap();
        write_private_file(&extensions_root().join("../loot.json"), "{}").unwrap();
        let pack = ExtensionPack {
            id: "jailbreak".into(),
            root: dir,
        };
        assert!(resolve_pack_path(&pack, "loot").is_none());
    });
}
