//! Mastery checks for VC-201-076: bind admitted extension manifests and
//! managed adapter artifacts to checksums, signatures where available, and
//! explicit trust policy; a changed artifact is quarantined until re-admitted.
//!
//! Every test name starts `vc_201_076_mastery_` so the vector's evidence is
//! enumerable with `cargo test -p susi-config vc_201_076`.

use crate::extensions::*;
use std::collections::BTreeMap;

fn with_temp_home<F: FnOnce()>(f: F) {
    let _guard = crate::env_test_lock();
    let tmp = std::env::temp_dir().join(format!(
        "vc076_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::create_dir_all(tmp.join(".susi"));
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("HOME", &tmp);
    env.set("XDG_CONFIG_HOME", tmp.join("config"));
    env.set("SUSI_XDG", "0");
    env.remove("SUSI_EXTENSION_PACK");
    invalidate_extension_caches();
    f();
    drop(env);
    invalidate_extension_caches();
    let _ = std::fs::remove_dir_all(tmp);
}

fn pack(id: &str, files: &[(&str, &str)]) -> ExtensionManifest {
    let mut m = ExtensionManifest {
        id: id.into(),
        name: id.into(),
        version: "1.0.0".into(),
        api_version: "1".into(),
        description: String::new(),
        capabilities: Vec::new(),
        requires: Vec::new(),
        optional: Vec::new(),
        permissions: Vec::new(),
        files: BTreeMap::new(),
        extra: Default::default(),
    };
    for (k, v) in files {
        m.files.insert(k.to_string(), v.to_string());
    }
    m
}

/// The claim requires manifests *bound to checksums* and "signatures where
/// available". `ExtensionManifest` has no checksum, digest, or signature
/// field at all — `files` maps names to relative paths only, and
/// `validate_manifest` never consults any content binding.
#[test]
fn vc_201_076_mastery_manifest_has_no_checksum_or_signature_fields() {
    let m = pack("p", &[("data.json", "data.json")]);
    let json = serde_json::to_value(&m).expect("serializes");
    let keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    for k in &keys {
        assert!(!k.contains("checksum"), "no checksum binding: {k}");
        assert!(!k.contains("digest"), "no digest binding: {k}");
        assert!(!k.contains("signature"), "no signature binding: {k}");
    }
    // An attacker may put a "checksum" under `extra`; validate ignores it.
    let mut m2 = pack("p", &[("data.json", "data.json")]);
    m2.extra.insert(
        "checksums".into(),
        serde_json::json!({"data.json": "sha256:00000000"}),
    );
    assert!(validate_manifest(&m2).is_ok());
}

/// "A changed artifact is quarantined until re-admitted": nothing tracks
/// artifact content after admission. Drop a pack, admit it, then tamper with
/// the admitted file — a second load succeeds with no quarantine, no error.
#[test]
fn vc_201_076_mastery_tampered_artifact_still_loads() {
    with_temp_home(|| {
        let root = extensions_root().join("evil");
        std::fs::create_dir_all(&root).unwrap();
        let m = pack("evil", &[("data.json", "data.json")]);
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_string_pretty(&m).unwrap(),
        )
        .unwrap();
        std::fs::write(root.join("data.json"), b"original").unwrap();
        load_pack("evil").expect("pack admits");
        // Tamper with the admitted artifact.
        std::fs::write(root.join("data.json"), b"TAMPERED-payload").unwrap();
        // Claim requires quarantine; reality: it loads again, unchanged.
        load_pack("evil").expect("tampered artifact loads unchallenged");
    });
}

/// The digest ledger that does exist is only for the *default* pack's reseed
/// heuristic, and tampering there is indistinguishable from "operator
/// customization": a mutated default-pack file survives `seed_default_pack`
/// and is re-recorded as the new current digest — the change is preserved,
/// not quarantined.
#[test]
fn vc_201_076_mastery_tampered_default_file_is_preserved_not_quarantined() {
    with_temp_home(|| {
        let root = seed_default_pack().expect("seeded");
        let target = root.join("cloud-vendors.json");
        std::fs::write(&target, b"{\"vendors\":[{\"id\":\"evil\"}]}").unwrap();
        seed_default_pack().expect("reseed keeps the tampered file");
        let text = std::fs::read_to_string(&target).unwrap();
        assert_eq!(text, "{\"vendors\":[{\"id\":\"evil\"}]}");
    });
}

/// The substrate state has only active/loaded/unloaded — there is no
/// quarantine lane for a changed artifact to sit in pending re-admission.
#[test]
fn vc_201_076_mastery_no_quarantine_state_exists() {
    with_temp_home(|| {
        let root = extensions_root().join("evil2");
        std::fs::create_dir_all(&root).unwrap();
        let m = pack("evil2", &[]);
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_string_pretty(&m).unwrap(),
        )
        .unwrap();
        load_pack("evil2").expect("admits");
        let state_text = std::fs::read_to_string(extensions_root().join("state.json")).unwrap();
        assert!(!state_text.contains("quarantine"));
        assert!(!state_text.contains("revoked"));
        assert!(!state_text.contains("trust"));
    });
}

/// Holds: shape validation refuses unknown permissions.
#[test]
fn vc_201_076_mastery_unknown_permission_rejected_holds() {
    let mut m = pack("p", &[]);
    m.permissions.push("admin.everything".into());
    assert!(validate_manifest(&m).is_err());
}

/// Holds: absolute-path file mappings are refused.
#[test]
fn vc_201_076_mastery_absolute_file_mapping_rejected_holds() {
    let m = pack("p", &[("data.json", "/etc/passwd")]);
    assert!(validate_manifest(&m).is_err());
}
