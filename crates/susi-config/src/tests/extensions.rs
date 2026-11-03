//! Tests for `extensions`.

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
    // Pre-create the legacy base so `susi_paths::SusiDirs::use_xdg()` cannot flip
    // mid-test if a concurrent test creates it under the swapped HOME.
    let _ = std::fs::create_dir_all(tmp.join(".susi"));
    // Also scrubs SUSI_HOME/SUSI_PORT_OFFSET; restored when `env` drops.
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("HOME", &tmp);
    env.set("XDG_CONFIG_HOME", tmp.join("config"));
    // Windows resolves the config root through APPDATA, not HOME: without this
    // the tests share the real profile, race on state.json and read each
    // other's truncated writes.
    env.set("APPDATA", tmp.join("appdata"));
    env.set("LOCALAPPDATA", tmp.join("localappdata"));
    env.set("SUSI_XDG", "0");
    env.remove("SUSI_EXTENSION_PACK");
    invalidate_extension_caches();
    f();
    drop(env);
    invalidate_extension_caches();
    let _ = std::fs::remove_dir_all(tmp);
}

/// A synthetic pack manifest built inside the test (core bundles none): it
/// maps catalogs through `../../` and declares `filesystem.read`, the shape
/// the old bundled default pack had.
fn synthetic_manifest() -> ExtensionManifest {
    serde_json::from_value(serde_json::json!({
        "id": "acme",
        "name": "Acme Pack",
        "version": "0.1.0",
        "apiVersion": "1",
        "capabilities": ["catalog.cloud_vendors", "catalog.mcp"],
        "permissions": ["filesystem.read"],
        "files": {
            "cloud-vendors.json": "cloud-vendors.json",
            "leading-mcp.json": "../../shared/leading-mcp.json"
        }
    }))
    .expect("synthetic manifest")
}

#[test]
fn synthetic_pack_manifest_validates() {
    let m = synthetic_manifest();
    assert_eq!(m.id, "acme");
    assert!(m.files.contains_key("cloud-vendors.json"));
    assert!(validate_manifest(&m).is_ok());
    assert_eq!(m.api_version.chars().next(), Some('1'));
}

#[test]
fn validate_manifest_rejects_incompatible_api_major() {
    let mut m = synthetic_manifest();
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

/// Zero-UI plan A9: core seeds no pack. A fresh host keeps no extensions
/// directory at all, and every reader serves compiled defaults.
#[test]
fn a_fresh_host_seeds_no_pack_and_writes_nothing() {
    with_temp_home(|| {
        let pack = ensure_extensions_substrate().expect("substrate");
        assert_eq!(pack.id, "default");
        assert!(!pack.root.exists(), "no default pack directory is seeded");
        assert!(!extensions_root().exists(), "no extensions root is created");
        assert!(!state_path().exists());
        assert!(list_packs().is_empty());
        assert_eq!(active_pack().id, "default");
        assert!(pack_file("cloud-vendors.json").is_none());
        // The launchable catalogs core no longer ships read as empty.
        let mcp: Vec<serde_json::Value> = load_json_or_bundled("leading-mcp.json", "[]");
        assert!(mcp.is_empty());
        // Compiled cloud-vendor aliases stay (NP-04).
        assert!(load_cloud_vendors().iter().any(|v| v.id == "openai"));
        assert!(!extensions_root().exists(), "reads write nothing");
    });
}

/// With no pack installed the managed-peer catalogs are empty, and the
/// adapter files the host (or the commercial desktop) writes under
/// `execution-agents/` and `agent-engines/` are admitted as managed peers.
#[test]
fn managed_peers_work_with_an_empty_catalog_and_pick_up_host_adapters() {
    with_temp_home(|| {
        assert!(crate::SusiConfig::default()
            .external_peer_agents()
            .is_empty());
        let dir = extensions_root()
            .parent()
            .unwrap()
            .join("managed-peers-fixture");
        assert!(crate::config::managed_peers_from(&dir).is_empty());
        std::fs::create_dir_all(dir.join("execution-agents")).unwrap();
        std::fs::create_dir_all(dir.join("agent-engines")).unwrap();
        std::fs::write(
            dir.join("execution-agents").join("acme-coder.json"),
            r#"{"kind":"command","program":"acme","args":["{prompt}"]}"#,
        )
        .unwrap();
        // Bookkeeping, not an adapter.
        std::fs::write(dir.join("execution-agents").join("ready.json"), "[]").unwrap();
        std::fs::write(
            dir.join("agent-engines").join("acme-flow.json"),
            r#"{"kind":"a2a","url":"https://agents.example.invalid"}"#,
        )
        .unwrap();
        let peers = crate::config::managed_peers_from(&dir);
        let names: Vec<_> = peers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["acme-coder", "acme-flow"]);
        assert!(peers.iter().all(|p| p.protocol == "managed"));

        // A pack entry plus a same-id host file (the per-id adapter override
        // `susi agents configure acme-coder` writes) is one peer, not two.
        let pack = extensions_root().join("peers");
        private_dir(&pack).unwrap();
        write_private_file(
            &pack.join("manifest.json"),
            r#"{"id":"peers","name":"Peers","version":"0.0.1",
                "files":{"execution-agents.json":"execution-agents.json"}}"#,
        )
        .unwrap();
        write_private_file(
            &pack.join("execution-agents.json"),
            r#"[{"id":"acme-coder","peer_name":"AcmeCoderAgent","name":"Acme Coder"}]"#,
        )
        .unwrap();
        load_pack("peers").unwrap();
        let peers = crate::config::managed_peers_from(&dir);
        let names: Vec<_> = peers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["AcmeCoderAgent", "acme-flow"]);
        unload_pack("peers").unwrap();
    });
}

/// CLI/HTTP/A2A peers ship in a pack as `external-peer-agents.json` (a bare
/// array of peer specs): admitted while the pack is loaded, gone after
/// unload.
#[test]
fn pack_external_peer_agents_are_admitted_while_loaded() {
    with_temp_home(|| {
        let names = || -> Vec<String> {
            crate::SusiConfig::default()
                .external_peer_agents()
                .into_iter()
                .map(|p| p.name)
                .collect()
        };
        assert!(names().is_empty());
        let pack = extensions_root().join("cli-peers");
        private_dir(&pack).unwrap();
        write_private_file(
            &pack.join("manifest.json"),
            r#"{"id":"cli-peers","name":"CLI peers","version":"0.0.1",
                "files":{"external-peer-agents.json":"external-peer-agents.json"}}"#,
        )
        .unwrap();
        write_private_file(
            &pack.join("external-peer-agents.json"),
            r#"[{"name":"AcmeCliAgent","protocol":"cli","detect_bins":["acme"],"args":["{goal}"]}]"#,
        )
        .unwrap();
        load_pack("cli-peers").unwrap();
        assert_eq!(names(), ["AcmeCliAgent"]);
        unload_pack("cli-peers").unwrap();
        assert!(names().is_empty());
    });
}

/// `default` means compiled defaults, also on an upgraded host that still
/// holds a legacy `extensions/default/` an earlier core seeded: unloading it
/// stops serving its files, and unloading the active pack never activates
/// another installed pack in its place.
#[test]
fn unload_falls_back_to_compiled_defaults_only() {
    with_temp_home(|| {
        let legacy = extensions_root().join("default");
        private_dir(&legacy).unwrap();
        write_private_file(
            &legacy.join("manifest.json"),
            r#"{"id":"default","name":"Legacy default","version":"0.0.1",
                "files":{"leading-mcp.json":"leading-mcp.json"}}"#,
        )
        .unwrap();
        write_private_file(&legacy.join("leading-mcp.json"), r#"[{"id":"legacy"}]"#).unwrap();
        invalidate_extension_caches();
        let rows: Vec<serde_json::Value> = load_json_or_bundled("leading-mcp.json", "[]");
        assert_eq!(rows.len(), 1, "an upgraded host serves its legacy pack");

        unload_pack("default").unwrap();
        assert!(pack_file("leading-mcp.json").is_none());
        let rows: Vec<serde_json::Value> = load_json_or_bundled("leading-mcp.json", "[]");
        assert!(rows.is_empty(), "unloaded: compiled defaults");
        assert!(list_packs()
            .iter()
            .any(|p| p.id == "default" && !p.active && !p.loaded));

        // Unloading the active pack does not activate another installed one.
        create_pack("aaa-old").unwrap();
        create_pack("zzz-new").unwrap();
        load_pack("zzz-new").unwrap();
        unload_pack("zzz-new").unwrap();
        assert_eq!(active_pack().id, "default");
        assert!(pack_file("leading-mcp.json").is_none());

        // An explicit load re-admits the legacy pack.
        load_pack("default").unwrap();
        assert!(pack_file("leading-mcp.json").is_some());
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
    let mut m = synthetic_manifest();
    m.permissions = vec!["kernel.root".into()];
    let err = validate_manifest(&m).expect_err("unknown permission must fail");
    assert!(err.contains("unknown permission"), "{err}");
}

#[test]
fn validate_manifest_rejects_escaping_files_without_permission() {
    let mut m = synthetic_manifest();
    m.permissions = Vec::new();
    m.files
        .insert("evil.json".into(), "../../outside.json".into());
    let err = validate_manifest(&m).expect_err("escape without filesystem.read must fail");
    assert!(err.contains("filesystem.read"), "{err}");
}

#[test]
fn validate_manifest_allows_escaping_files_with_filesystem_read() {
    // A pack that maps catalogs via `../../` and declares filesystem.read
    // must keep validating.
    let m = synthetic_manifest();
    assert!(
        m.files.values().any(|v| v.starts_with("../")),
        "synthetic manifest uses parent-relative files"
    );
    assert!(validate_manifest(&m).is_ok());
}

#[test]
fn validate_manifest_rejects_absolute_file_paths_even_with_permission() {
    let mut m = synthetic_manifest();
    // A Unix path is not absolute to Windows path resolution, so the fixture
    // must be platform-appropriate for the rejection to be the thing tested.
    let absolute = if cfg!(windows) {
        "C:\\Windows\\win.ini"
    } else {
        "/etc/passwd"
    };
    m.files.insert("abs.json".into(), absolute.into());
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
        let manifest: ExtensionManifest =
            serde_json::from_slice(&std::fs::read(pack.root.join("manifest.json")).unwrap())
                .unwrap();
        assert!(resolve_pack_path(&pack, &manifest, "loot").is_none());
    });
}

#[test]
fn external_manifest_must_parse_and_match_the_directory_even_when_forced() {
    with_temp_home(|| {
        let root = extensions_root().join("external");
        private_dir(&root).unwrap();
        for body in ["{", r#"{"id":"default","name":"External","version":"1"}"#] {
            write_private_file(&root.join("manifest.json"), body).unwrap();
            assert!(load_pack("external").is_err());
            assert!(readmit_pack("external").is_err());
            let mut env = susi_paths::test_env::EnvGuard::isolated();
            env.set("SUSI_EXTENSION_PACK", "external");
            assert_eq!(active_pack().id, "default");
            assert!(!list_packs().iter().any(|p| p.id == "external" && p.loaded));
        }
        for id in [
            "../external",
            "..\\external",
            "C:elsewhere",
            "CON",
            "hidden.",
            "data:stream",
        ] {
            assert!(create_pack(id).is_err());
            assert!(load_pack(id).is_err());
            assert!(readmit_pack(id).is_err());
        }
    });
}

#[test]
fn external_pack_consumes_only_declared_bytes_even_with_a_shadowing_direct_file() {
    with_temp_home(|| {
        let root = extensions_root().join("external");
        private_dir(&root).unwrap();
        let manifest = |files: &str| {
            format!(r#"{{"id":"external","name":"External","version":"1","files":{files}}}"#)
        };
        let overlay = r#"[{"id":"shadow","aliases":["shadow"],"api_key_env":"SHADOW_KEY"}]"#;
        write_private_file(&root.join("cloud-vendors.json"), overlay).unwrap();
        write_private_file(&root.join("manifest.json"), &manifest("{}")).unwrap();
        load_pack("external").unwrap();
        assert!(pack_file("cloud-vendors.json").is_none());
        assert!(!load_cloud_vendors().iter().any(|v| v.id == "shadow"));

        let declared = root.join("catalogs/vendors.json");
        write_private_file(&declared, &overlay.replace("shadow", "declared")).unwrap();
        write_private_file(
            &root.join("manifest.json"),
            &manifest(r#"{"cloud-vendors.json":"catalogs/vendors.json"}"#),
        )
        .unwrap();
        readmit_pack("external").unwrap();
        assert_eq!(load_cloud_vendors()[0].id, "declared");
        assert!(pack_file("../external/cloud-vendors.json").is_none());
        write_private_file(&declared, overlay).unwrap();
        assert!(!load_cloud_vendors()
            .iter()
            .any(|v| v.id == "declared" || v.id == "shadow"));
        assert!(list_packs()
            .iter()
            .any(|p| p.id == "external" && p.quarantined));
    });
}

#[cfg(any(windows, unix))]
#[test]
#[cfg_attr(
    windows,
    ignore = "requires the Windows symlink privilege or Developer Mode"
)]
fn external_pack_cannot_read_through_a_redirected_directory_or_file() {
    #[cfg(unix)]
    use std::os::unix::fs::{symlink as symlink_dir, symlink as symlink_file};
    #[cfg(windows)]
    use std::os::windows::fs::{symlink_dir, symlink_file};
    with_temp_home(|| {
        private_dir(&extensions_root()).unwrap();
        let outside = extensions_root().parent().unwrap().join("other");
        private_dir(&outside).unwrap();
        let root = extensions_root().join("external");
        let body = r#"{"id":"external","name":"External","version":"1","permissions":["filesystem.read"],"files":{"x":"data"}}"#;
        write_private_file(&outside.join("manifest.json"), body).unwrap();
        write_private_file(&outside.join("data"), "[]").unwrap();
        symlink_dir(&outside, &root).unwrap();
        assert!(load_pack("external").is_err());
        #[cfg(unix)]
        std::fs::remove_file(&root).unwrap();
        #[cfg(windows)]
        std::fs::remove_dir(&root).unwrap();
        private_dir(&root).unwrap();
        write_private_file(&root.join("manifest.json"), body).unwrap();
        symlink_file(outside.join("data"), root.join("data")).unwrap();
        assert!(load_pack("external").is_err());
    });
}

#[test]
fn external_pack_with_permission_can_use_an_explicit_relative_mapping() {
    with_temp_home(|| {
        let root = extensions_root().join("external");
        private_dir(&root).unwrap();
        write_private_file(&root.join("manifest.json"), r#"{"id":"external","name":"External","version":"1","permissions":["filesystem.read"],"files":{"data":"../shared.json"}}"#).unwrap();
        write_private_file(&extensions_root().join("shared.json"), r#"["declared"]"#).unwrap();
        load_pack("external").unwrap();
        assert_eq!(
            load_json_or_bundled::<Vec<String>>("data", "[]"),
            ["declared"]
        );
    });
}

#[test]
fn credential_redaction_covers_governance_tokens_and_env_secrets() {
    with_temp_home(|| {
        let env_secret = "not-a-token-shape-but-secret-123";
        unsafe {
            std::env::set_var("SUSI_REDACT_PROBE_API_KEY", env_secret);
        }
        let text = format!(
            "google=AIzaSyA1b2C3 pat=github_pat_11ABCdef slack=xoxb-123-abc env={env_secret} ok=plain"
        );
        let out = crate::redact_credentials(&text);
        unsafe {
            std::env::remove_var("SUSI_REDACT_PROBE_API_KEY");
        }
        for leaked in [
            "AIzaSyA1b2C3",
            "github_pat_11ABCdef",
            "xoxb-123-abc",
            env_secret,
        ] {
            assert!(!out.contains(leaked), "{leaked} leaked: {out}");
        }
        assert!(out.contains("ok=plain"), "{out}");
    });
}

#[test]
fn cloud_env_keys_are_read_from_file_without_touching_process_env() {
    with_temp_home(|| {
        let path = crate::cloud_env::cloud_env_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "SUSI_PROBE_FILE_ONLY_KEY=from-file\nSUSI_PROBE_SHADOWED_KEY=from-file\n",
        )
        .unwrap();
        unsafe {
            std::env::set_var("SUSI_PROBE_SHADOWED_KEY", "from-env");
        }
        let file_only = crate::env_or_cloud_env("SUSI_PROBE_FILE_ONLY_KEY");
        let shadowed = crate::env_or_cloud_env("SUSI_PROBE_SHADOWED_KEY");
        let missing = crate::env_or_cloud_env("SUSI_PROBE_ABSENT_KEY");
        let overlay = crate::cloud_env_overlay();
        let in_env = std::env::var_os("SUSI_PROBE_FILE_ONLY_KEY");
        unsafe {
            std::env::remove_var("SUSI_PROBE_SHADOWED_KEY");
        }
        assert_eq!(file_only.as_deref(), Ok("from-file"));
        assert_eq!(shadowed.as_deref(), Ok("from-env"));
        assert!(missing.is_err());
        assert!(in_env.is_none(), "lookup must not copy keys into the env");
        assert!(overlay.contains(&("SUSI_PROBE_FILE_ONLY_KEY".into(), "from-file".into())));
        assert!(!overlay.iter().any(|(k, _)| k == "SUSI_PROBE_SHADOWED_KEY"));
    });
}

fn modified(path: &std::path::Path) -> std::time::SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

/// A moment well after every file a test wrote: no entry is still racy.
fn a_minute_later() -> std::time::SystemTime {
    std::time::SystemTime::now() + std::time::Duration::from_secs(60)
}

fn decisions() -> usize {
    ENSURE_DECISIONS.with(std::cell::Cell::get)
}

/// Every catalog read re-derives the substrate. Once it has settled nothing
/// is rewritten: each rewrite was a synced temp file and a rename, hundreds
/// of them per CLI command.
#[test]
fn a_settled_substrate_is_not_rewritten() {
    with_temp_home(|| {
        create_pack("mine").unwrap();
        ensure_extensions_substrate().unwrap();
        let manifest = extensions_root().join("mine").join("manifest.json");
        let before = (modified(&state_path()), modified(&manifest));
        // Past the filesystem's clock granularity, so a rewrite would show.
        std::thread::sleep(std::time::Duration::from_millis(30));
        // The full decision, not the remembered answer.
        invalidate_extension_caches();
        let decided = decisions();
        ensure_extensions_substrate().unwrap();
        assert_eq!(decisions(), decided + 1, "the substrate was decided again");
        assert_eq!((modified(&state_path()), modified(&manifest)), before);
    });
}

/// A pack that fails its integrity check is quarantined once; the catalog
/// reads after that do not rewrite the state to record it again.
#[test]
fn a_standing_quarantine_is_not_rewritten_by_every_read() {
    with_temp_home(|| {
        // A synthetic installed pack, admitted, then edited after admission.
        let root = extensions_root().join("acme");
        private_dir(&root).unwrap();
        write_private_file(
            &root.join("manifest.json"),
            r#"{"id":"acme","name":"Acme","version":"1","files":{"leading-mcp.json":"leading-mcp.json"}}"#,
        )
        .unwrap();
        write_private_file(&root.join("leading-mcp.json"), "[2]").unwrap();
        load_pack("acme").unwrap();
        let tampered = root.join("leading-mcp.json");
        std::fs::write(&tampered, "[]").unwrap();
        let first: Vec<serde_json::Value> = load_json_or_bundled("leading-mcp.json", "[1]");
        assert_eq!(
            first,
            vec![serde_json::json!(1)],
            "the bundled catalog serves"
        );
        // The second read records the standing quarantine in place of the
        // first read's reason; from then on the state is settled.
        let _: Vec<serde_json::Value> = load_json_or_bundled("leading-mcp.json", "[1]");
        let state = state_path();
        let recorded = std::fs::read_to_string(&state).unwrap();
        assert!(recorded.contains("quarantined"), "{recorded}");
        let before = modified(&state);
        std::thread::sleep(std::time::Duration::from_millis(30));
        let _: Vec<serde_json::Value> = load_json_or_bundled("leading-mcp.json", "[1]");
        assert_eq!(
            std::fs::read_to_string(&state).unwrap(),
            recorded,
            "a standing quarantine is not re-recorded"
        );
        assert_eq!(modified(&state), before);
    });
}

/// The answer is remembered while nothing it read has changed, and decided
/// afresh the moment something has — here another process activating a
/// pack by editing the state file, which no in-process invalidation
/// announces.
#[test]
fn the_remembered_pack_follows_an_edit_on_disk() {
    with_temp_home(|| {
        create_pack("mine").unwrap();
        let later = a_minute_later();
        assert_eq!(ensure_extensions_substrate_at(later).unwrap().id, "default");
        assert_eq!(ensure_extensions_substrate_at(later).unwrap().id, "default");
        let decided = decisions();
        assert_eq!(ensure_extensions_substrate_at(later).unwrap().id, "default");
        assert_eq!(
            decisions(),
            decided,
            "an unchanged substrate is not decided again"
        );
        let state = std::fs::read_to_string(state_path()).unwrap();
        let edited = state.replace("\"active\": \"default\"", "\"active\": \"mine\"");
        assert_ne!(state, edited);
        std::fs::write(state_path(), edited).unwrap();
        assert_eq!(ensure_extensions_substrate_at(later).unwrap().id, "mine");
        // A new pack directory is seen the same way.
        let ours = extensions_root().join("ours");
        private_dir(&ours).unwrap();
        std::fs::write(
            ours.join("manifest.json"),
            r#"{"id": "ours", "name": "Ours", "version": "0.0.1"}"#,
        )
        .unwrap();
        let decided = decisions();
        ensure_extensions_substrate_at(later).unwrap();
        assert_eq!(decisions(), decided + 1, "a new pack was not looked at");
        let state = std::fs::read_to_string(state_path()).unwrap();
        assert!(state.contains("\"ours\""), "{state}");
    });
}

/// The state file hard-linked from another directory (a dotfiles copy) and
/// edited through that other link. On Windows the extensions directory's
/// own listing keeps the old size and time for such a write, so a stamp
/// built from it would not change and the old pack would be answered from
/// memory; the stamp stats each path, which sees the write.
#[test]
fn an_edit_through_a_hard_link_elsewhere_is_seen() {
    with_temp_home(|| {
        create_pack("mine").unwrap();
        let later = a_minute_later();
        assert_eq!(ensure_extensions_substrate_at(later).unwrap().id, "default");
        let dotfiles = extensions_root().parent().unwrap().join("dotfiles");
        std::fs::create_dir_all(&dotfiles).unwrap();
        let other = dotfiles.join("state.json");
        std::fs::hard_link(state_path(), &other).unwrap();
        // Remembered while nothing changed.
        assert_eq!(ensure_extensions_substrate_at(later).unwrap().id, "default");
        let decided = decisions();
        assert_eq!(ensure_extensions_substrate_at(later).unwrap().id, "default");
        assert_eq!(decisions(), decided);
        let state = std::fs::read_to_string(&other).unwrap();
        let edited = state.replace("\"active\": \"default\"", "\"active\": \"mine\"");
        assert_ne!(state, edited);
        std::fs::write(&other, edited).unwrap();
        assert_eq!(ensure_extensions_substrate_at(later).unwrap().id, "mine");
    });
}

/// A file rewritten within one timestamp tick at the same size keeps its
/// size and modification time: a stamp taken that soon after a write is not
/// trusted, so the rewrite is read rather than answered from memory.
#[test]
fn a_rewrite_within_one_timestamp_tick_is_not_hidden() {
    with_temp_home(|| {
        create_pack("mine").unwrap();
        create_pack("ours").unwrap();
        let activate = |id: &str, at: std::time::SystemTime| {
            let state = std::fs::read_to_string(state_path()).unwrap();
            let active = serde_json::from_str::<serde_json::Value>(&state).unwrap()["active"]
                .as_str()
                .unwrap()
                .to_string();
            let edited = state.replace(
                &format!("\"active\": \"{active}\""),
                &format!("\"active\": \"{id}\""),
            );
            std::fs::write(state_path(), edited).unwrap();
            std::fs::File::options()
                .write(true)
                .open(state_path())
                .unwrap()
                .set_modified(at)
                .unwrap();
        };
        // Everything else was written a minute before `tick`.
        let tick = a_minute_later();
        activate("mine", tick);
        let just_after = tick + std::time::Duration::from_millis(1);
        assert_eq!(
            ensure_extensions_substrate_at(just_after).unwrap().id,
            "mine"
        );
        // Same size, same modification time, other bytes.
        activate("ours", tick);
        assert_eq!(
            ensure_extensions_substrate_at(just_after).unwrap().id,
            "ours"
        );
    });
}

/// The state file replaced by another file of the same size whose
/// modification time was set back to the old one's (`cp -p`, `rsync -a`,
/// a restore from backup): name, size and mtime all match, but it is
/// another file, and the stamp's file identity sees that.
#[test]
fn a_replaced_file_with_the_old_size_and_mtime_is_seen() {
    with_temp_home(|| {
        // As long a name as "default", so the edit keeps the file's size.
        let target = "seventh";
        create_pack(target).unwrap();
        let later = a_minute_later();
        assert_eq!(ensure_extensions_substrate_at(later).unwrap().id, "default");
        let state = std::fs::read_to_string(state_path()).unwrap();
        let edited = state.replace("\"active\": \"default\"", "\"active\": \"seventh\"");
        assert_ne!(edited, state);
        assert_eq!(edited.len(), state.len());
        let decided = decisions();
        assert_eq!(ensure_extensions_substrate_at(later).unwrap().id, "default");
        assert_eq!(decisions(), decided, "remembered while unchanged");

        let old = std::fs::metadata(state_path()).unwrap();
        let staged = state_path().with_extension("restored");
        std::fs::write(&staged, &edited).unwrap();
        let times = std::fs::FileTimes::new().set_modified(old.modified().unwrap());
        // A restore gives the file its own creation time.
        #[cfg(windows)]
        let times = {
            use std::os::windows::fs::FileTimesExt;
            times.set_created(old.created().unwrap() - std::time::Duration::from_secs(3600))
        };
        std::fs::File::options()
            .write(true)
            .open(&staged)
            .unwrap()
            .set_times(times)
            .unwrap();
        std::fs::rename(&staged, state_path()).unwrap();
        let now = std::fs::metadata(state_path()).unwrap();
        assert_eq!(now.len(), old.len());
        assert_eq!(now.modified().unwrap(), old.modified().unwrap());
        assert_eq!(ensure_extensions_substrate_at(later).unwrap().id, target);
    });
}

/// A decision that warns (here about a pack the host cannot load) warns on
/// every call until the operator acts, as it always did: it is taken again
/// each time, never answered from memory.
#[test]
fn a_decision_that_warns_is_taken_on_every_call() {
    with_temp_home(|| {
        ensure_extensions_substrate().unwrap();
        let bad = extensions_root().join("badapi");
        private_dir(&bad).unwrap();
        std::fs::write(
            bad.join("manifest.json"),
            r#"{"id": "badapi", "name": "Bad", "version": "0.0.1", "apiVersion": "99"}"#,
        )
        .unwrap();
        let later = a_minute_later();
        ensure_extensions_substrate_at(later).unwrap();
        let decided = decisions();
        ensure_extensions_substrate_at(later).unwrap();
        ensure_extensions_substrate_at(later).unwrap();
        assert_eq!(decisions(), decided + 2);
    });
}

#[test]
fn damaged_state_is_never_rewritten_or_reset() {
    with_temp_home(|| {
        private_dir(&extensions_root()).unwrap();
        std::fs::write(state_path(), "{\"active\":\"my-pack\",\"unloaded\":[").unwrap();
        // Zero-config startup falls back to compiled defaults in memory…
        let pack = ensure_extensions_substrate().unwrap();
        assert_eq!(pack.id, "default");
        // …without overwriting the operator's damaged state.
        assert_eq!(
            std::fs::read_to_string(state_path()).unwrap(),
            "{\"active\":\"my-pack\",\"unloaded\":["
        );
        // Explicit state changes refuse instead of resetting it.
        assert!(unload_pack("default").is_err());
        assert_eq!(
            std::fs::read_to_string(state_path()).unwrap(),
            "{\"active\":\"my-pack\",\"unloaded\":["
        );
    });
}

fn author_pack(
    name: &str,
    manifest: &serde_json::Value,
    files: &[(&str, &str)],
) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "susi_ext_author_{name}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("manifest.json"), manifest.to_string()).unwrap();
    for (rel, body) in files {
        std::fs::write(root.join(rel), body).unwrap();
    }
    root
}

#[test]
fn validate_pack_dir_applies_host_admission_rules_to_author_packs() {
    use sha2::{Digest, Sha256};
    let body = r#"{"vendors":[]}"#;
    let digest: String = Sha256::digest(body.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let base = serde_json::json!({
        "id": "acme-pack", "name": "Acme", "version": "1.0.0", "apiVersion": "1",
        "capabilities": [], "permissions": [],
        "files": {"cloud-vendors.json": "cloud-vendors.json"},
        "checksums": {"cloud-vendors.json": digest},
    });
    let ok = author_pack("ok", &base, &[("cloud-vendors.json", body)]);
    let (manifest, artifacts) = validate_pack_dir(&ok).unwrap();
    assert_eq!(manifest.id, "acme-pack");
    assert_eq!(artifacts["cloud-vendors.json"], format!("sha256:{digest}"));
    assert!(artifacts.contains_key("manifest.json"));

    let mut tampered = author_pack("tampered", &base, &[("cloud-vendors.json", "{}")]);
    assert!(validate_pack_dir(&tampered)
        .unwrap_err()
        .contains("checksum mismatch"));
    std::fs::remove_dir_all(&tampered).ok();

    let mut unknown = base.clone();
    unknown["permissions"] = serde_json::json!(["root.everything"]);
    tampered = author_pack("perm", &unknown, &[("cloud-vendors.json", body)]);
    assert!(validate_pack_dir(&tampered)
        .unwrap_err()
        .contains("unknown permission"));
    std::fs::remove_dir_all(&tampered).ok();

    let mut escape = base.clone();
    escape["files"] = serde_json::json!({"cloud-vendors.json": "../outside.json"});
    escape["checksums"] = serde_json::json!({});
    tampered = author_pack("escape", &escape, &[]);
    assert!(validate_pack_dir(&tampered).is_err());
    std::fs::remove_dir_all(&tampered).ok();

    let broken = author_pack("broken", &serde_json::json!({"id": 7}), &[]);
    assert!(validate_pack_dir(&broken)
        .unwrap_err()
        .starts_with("invalid manifest"));
    std::fs::remove_dir_all(&broken).ok();
    std::fs::remove_dir_all(&ok).ok();
}
