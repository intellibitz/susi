#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! VC-201-076: Verify extension and adapter supply-chain provenance.
//!
//! Mastery criteria:
//! 1. Checksum binding: ExtensionManifest binds declared files to sha256 checksums;
//!    `validate_manifest` enforces valid digest format and rejects malformed values.
//! 2. Ed25519 signatures: Optional public-key signatures are verified over file bytes;
//!    tampering breaks signature verification and triggers quarantine.
//! 3. Quarantine and re-admission lifecycle:
//!    - Tampered admitted artifacts are quarantined on load failure.
//!    - `list_packs()` reports `quarantined == true`.
//!    - Explicit `readmit_pack()` re-admits the current payload and permits subsequent loads.
//! 4. Tampered default seeded files are prevented from being served by `pack_file()`.
//! 5. Quarantine state and trusted digests persist in `state.json`.

mod common;

use common::HomeGuard;
use ed25519_dalek::Signer;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use susi_config::extensions::*;

fn compute_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", susi_paths::hex::encode(Sha256::digest(bytes)))
}

fn extensions_root() -> PathBuf {
    susi_paths::SusiDirs::config_dir().join("extensions")
}

fn with_temp_home<F: FnOnce()>(f: F) {
    let _home = HomeGuard::new("vc076");
    invalidate_extension_caches();
    f();
    invalidate_extension_caches();
}

fn create_pack(id: &str, files: &[(&str, &str)]) -> ExtensionManifest {
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
fn vc_201_076_mastery() {
    // 1. Checksum validation in ExtensionManifest
    let mut m = create_pack("pkg-checksum", &[("config.json", "config.json")]);
    m.checksums
        .insert("config.json".into(), compute_digest(b"valid payload"));
    assert!(
        validate_manifest(&m).is_ok(),
        "valid sha256 checksum passes validation"
    );

    m.checksums
        .insert("config.json".into(), "not-a-valid-sha256".into());
    assert!(
        validate_manifest(&m).is_err(),
        "invalid digest format must fail manifest validation"
    );

    // 2. Ed25519 signature validation and tampering detection
    with_temp_home(|| {
        let root = extensions_root().join("signed_ext");
        std::fs::create_dir_all(&root).expect("create ext dir");
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
        let mut signed_manifest = create_pack("signed_ext", &[("model.json", "model.json")]);
        signed_manifest.signer = Some(susi_paths::hex::encode(
            signing_key.verifying_key().to_bytes(),
        ));
        signed_manifest.signatures.insert(
            "model.json".into(),
            susi_paths::hex::encode(signing_key.sign(b"original-model-weights").to_bytes()),
        );

        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_string_pretty(&signed_manifest).expect("serialize manifest"),
        )
        .expect("write manifest");
        std::fs::write(root.join("model.json"), b"original-model-weights")
            .expect("write model.json");

        // First load succeeds under valid signature
        let loaded = load_pack("signed_ext").expect("valid signature admits extension");
        assert!(loaded.loaded && loaded.active);

        // Tamper with model.json
        std::fs::write(root.join("model.json"), b"tampered-malicious-weights")
            .expect("tamper model.json");
        let tamper_err = load_pack("signed_ext")
            .expect_err("tampering with signed artifact must reject load and quarantine");
        assert!(
            tamper_err.contains("quarantined") || tamper_err.contains("mismatch"),
            "error indicates quarantine or signature mismatch: {tamper_err}"
        );

        // Verify quarantine status in list_packs
        let packs = list_packs();
        let ext_status = packs
            .iter()
            .find(|p| p.id == "signed_ext")
            .expect("signed_ext found in packs list");
        assert!(ext_status.quarantined, "tampered pack must be quarantined");

        // Attempting to re-admit an invalidly signed payload fails signature check
        assert!(
            readmit_pack("signed_ext").is_err(),
            "cannot re-admit an invalidly signed payload"
        );

        // Restoring valid payload allows readmit_pack to verify signature and re-admit
        std::fs::write(root.join("model.json"), b"original-model-weights")
            .expect("restore model.json");
        readmit_pack("signed_ext").expect("valid signature allows re-admission");
        let reloaded = load_pack("signed_ext").expect("re-admitted pack loads cleanly");
        assert!(reloaded.loaded && !reloaded.quarantined);

        // Test unsigned pack tampering and digest re-admission
        let unsigned_root = extensions_root().join("unsigned_ext");
        std::fs::create_dir_all(&unsigned_root).expect("create unsigned ext dir");
        let unsigned_manifest = create_pack("unsigned_ext", &[("data.json", "data.json")]);
        std::fs::write(
            unsigned_root.join("manifest.json"),
            serde_json::to_string_pretty(&unsigned_manifest).expect("serialize manifest"),
        )
        .expect("write manifest");
        std::fs::write(unsigned_root.join("data.json"), b"original-data").expect("write data.json");
        load_pack("unsigned_ext").expect("first load admits unsigned pack");

        // Tamper with data.json
        std::fs::write(unsigned_root.join("data.json"), b"tampered-data")
            .expect("tamper data.json");
        let tamper_err =
            load_pack("unsigned_ext").expect_err("tampered data must fail and quarantine");
        assert!(tamper_err.contains("quarantined") || tamper_err.contains("mismatch"));
        assert!(list_packs()
            .into_iter()
            .any(|p| p.id == "unsigned_ext" && p.quarantined));

        // Re-admit updates the trusted digest and admits the modified file
        readmit_pack("unsigned_ext").expect("explicit inspection re-admits updated digest");
        let reloaded_unsigned = load_pack("unsigned_ext").expect("re-admitted unsigned pack loads");
        assert!(reloaded_unsigned.loaded && !reloaded_unsigned.quarantined);

        // 3. A tampered file of the active installed pack is not served
        // (core seeds no default pack; `unsigned_ext` is the active one).
        assert!(
            pack_file("data.json").is_some(),
            "the admitted active pack file is served"
        );
        std::fs::write(unsigned_root.join("data.json"), b"tampered-again")
            .expect("tamper data.json again");
        assert!(
            pack_file("data.json").is_none(),
            "a tampered active pack file must not be served"
        );

        // 4. Quarantine state and trusted digests persist in state.json
        let state_path = extensions_root().join("state.json");
        let state_text = std::fs::read_to_string(&state_path).expect("read state.json");
        assert!(
            state_text.contains("trusted_digests"),
            "state.json must track trusted_digests"
        );
    });
}

#[test]
fn vc_201_076_validate_manifest_rejects_short_or_non_hex_sha256_digests() {
    let mut short_digest = create_pack("pkg-short", &[("a.json", "a.json")]);
    short_digest
        .checksums
        .insert("a.json".into(), "sha256:deadbeef".into());
    assert!(validate_manifest(&short_digest).is_err());

    let mut non_hex = create_pack("pkg-nonhex", &[("a.json", "a.json")]);
    non_hex
        .checksums
        .insert("a.json".into(), format!("sha256:{}", "z".repeat(64)));
    assert!(validate_manifest(&non_hex).is_err());
}

#[test]
fn vc_201_076_validate_manifest_enforces_signer_and_signature_hex_format() {
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let valid_sig = susi_paths::hex::encode(signing_key.sign(b"payload").to_bytes());
    let valid_signer = susi_paths::hex::encode(signing_key.verifying_key().to_bytes());

    let mut missing_signer = create_pack("pkg-nosigner", &[("a.json", "a.json")]);
    missing_signer.signatures.insert("a.json".into(), valid_sig);
    let err = validate_manifest(&missing_signer).expect_err("signatures without signer must fail");
    assert!(err.contains("without a signer"));

    let mut bad_signer = missing_signer.clone();
    bad_signer.signer = Some("not-32-byte-hex".into());
    assert!(validate_manifest(&bad_signer).is_err());

    let mut valid_signed = missing_signer;
    valid_signed.signer = Some(valid_signer);
    assert!(validate_manifest(&valid_signed).is_ok());
}

#[test]
fn vc_201_076_validate_manifest_enforces_file_bindings_and_permission_boundaries() {
    let mut unbound = create_pack("pkg-unbound", &[("a.json", "a.json")]);
    unbound
        .checksums
        .insert("missing.json".into(), compute_digest(b"abc"));
    let err = validate_manifest(&unbound).expect_err("checksum for undeclared file must fail");
    assert!(err.contains("binds unknown file"));

    let mut escaping = create_pack("pkg-escape", &[("shared.json", "../outside/shared.json")]);
    assert!(
        validate_manifest(&escaping).is_err(),
        "escaping pack root without filesystem.read must fail"
    );
    escaping.permissions.push("filesystem.read".into());
    assert!(
        validate_manifest(&escaping).is_ok(),
        "declaring filesystem.read permits parent-relative path"
    );
}

#[test]
fn vc_201_076_compute_digest_produces_64_char_hex_sha256_prefix() {
    let d1 = compute_digest(b"hello-world");
    let d2 = compute_digest(b"hello-world");
    let d3 = compute_digest(b"other-world");
    assert_eq!(d1, d2);
    assert_ne!(d1, d3);
    assert!(d1.starts_with("sha256:"));
    assert_eq!(d1.len(), "sha256:".len() + 64);
}
