//! Mastery checks for VC-201-076: bind admitted extension manifests and
//! managed adapter artifacts to checksums, signatures where available, and
//! explicit trust policy; a changed artifact is quarantined until re-admitted.
//!
//! Every test name starts `vc_201_076_mastery_` so the vector's evidence is
//! enumerable with `cargo test -p susi-config vc_201_076`.

use crate::extensions::*;
use std::collections::BTreeMap;

fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

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
        checksums: BTreeMap::new(),
        signatures: BTreeMap::new(),
        signer: None,
        extra: Default::default(),
    };
    for (k, v) in files {
        m.files.insert(k.to_string(), v.to_string());
    }
    m
}

#[test]
fn vc_201_076_mastery_manifest_binds_artifacts_to_checksums() {
    let mut m = pack("p", &[("data.json", "data.json")]);
    m.checksums.insert("data.json".into(), digest(b"original"));
    assert!(validate_manifest(&m).is_ok());
    m.checksums
        .insert("data.json".into(), "not-a-digest".into());
    assert!(validate_manifest(&m).is_err());
}

#[test]
fn vc_201_076_mastery_validates_optional_ed25519_signatures() {
    use ed25519_dalek::Signer;

    with_temp_home(|| {
        let root = extensions_root().join("signed");
        std::fs::create_dir_all(&root).unwrap();
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let mut m = pack("signed", &[("data.json", "data.json")]);
        m.signer = Some(hex::encode(signing_key.verifying_key().to_bytes()));
        m.signatures.insert(
            "data.json".into(),
            hex::encode(signing_key.sign(b"original").to_bytes()),
        );
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_string_pretty(&m).unwrap(),
        )
        .unwrap();
        std::fs::write(root.join("data.json"), b"original").unwrap();
        load_pack("signed").expect("valid signature admits");
        std::fs::write(root.join("data.json"), b"tampered").unwrap();
        let err = load_pack("signed").expect_err("tampering breaks the signature");
        assert!(err.contains("quarantined") || err.contains("mismatch"));
    });
}

#[test]
fn vc_201_076_mastery_tampered_artifact_is_quarantined_until_readmitted() {
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
        let err = load_pack("evil").expect_err("tampered artifact must quarantine");
        assert!(err.contains("quarantined") || err.contains("mismatch"));
        assert!(list_packs()
            .into_iter()
            .any(|status| status.id == "evil" && status.quarantined));
        readmit_pack("evil").expect("explicit inspection re-admits current bytes");
        load_pack("evil").expect("re-admitted artifact loads");
    });
}

#[test]
fn vc_201_076_mastery_tampered_default_file_is_not_served() {
    with_temp_home(|| {
        let root = seed_default_pack().expect("seeded");
        let target = root.join("cloud-vendors.json");
        std::fs::write(&target, b"{\"vendors\":[{\"id\":\"evil\"}]}").unwrap();
        seed_default_pack().expect("reseed does not overwrite operator bytes");
        assert!(pack_file("cloud-vendors.json").is_none());
    });
}

#[test]
fn vc_201_076_mastery_quarantine_is_persisted_in_state() {
    with_temp_home(|| {
        let root = extensions_root().join("evil2");
        std::fs::create_dir_all(&root).unwrap();
        let m = pack("evil2", &[("data.json", "data.json")]);
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_string_pretty(&m).unwrap(),
        )
        .unwrap();
        std::fs::write(root.join("data.json"), b"initial").unwrap();
        load_pack("evil2").expect("admits");
        std::fs::write(root.join("data.json"), b"changed").unwrap();
        let _ = load_pack("evil2");
        let state_text = std::fs::read_to_string(extensions_root().join("state.json")).unwrap();
        assert!(state_text.contains("quarantined"));
        assert!(state_text.contains("trusted_digests"));
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
