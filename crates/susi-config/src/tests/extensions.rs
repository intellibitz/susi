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
        assert!(resolve_pack_path(&pack, "loot").is_none());
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
        ensure_extensions_substrate().unwrap();
        let digests = extensions_root()
            .join("default")
            .join(".bundled-digests.json");
        let before = (modified(&state_path()), modified(&digests));
        // Past the filesystem's clock granularity, so a rewrite would show.
        std::thread::sleep(std::time::Duration::from_millis(30));
        // The full decision, not the remembered answer.
        invalidate_extension_caches();
        let decided = decisions();
        ensure_extensions_substrate().unwrap();
        assert_eq!(decisions(), decided + 1, "the substrate was decided again");
        assert_eq!((modified(&state_path()), modified(&digests)), before);
    });
}

/// A pack that fails its integrity check is quarantined once; the catalog
/// reads after that do not rewrite the state to record it again.
#[test]
fn a_standing_quarantine_is_not_rewritten_by_every_read() {
    with_temp_home(|| {
        ensure_extensions_substrate().unwrap();
        // An operator edit the pack's checksums do not cover.
        let tampered = extensions_root().join("default").join("leading-mcp.json");
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
        let recorded = std::fs::read_to_string(state_path()).unwrap();
        assert!(recorded.contains("quarantined"), "{recorded}");
        let before = modified(&state_path());
        std::thread::sleep(std::time::Duration::from_millis(30));
        let _: Vec<serde_json::Value> = load_json_or_bundled("leading-mcp.json", "[1]");
        assert_eq!(modified(&state_path()), before);
        assert_eq!(std::fs::read_to_string(state_path()).unwrap(), recorded);
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
        ensure_extensions_substrate().unwrap();
        std::fs::write(state_path(), "{\"active\":\"my-pack\",\"unloaded\":[").unwrap();
        // Zero-config startup falls back to the default pack in memory…
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
