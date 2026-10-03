//! Mastery checks for VC-201-060: round-trip a portable deployment spec
//! (endpoints, runtime requirements, secret references, placement policies)
//! between local and cloud targets, with compatibility checks surfacing
//! nonportable settings before any changes.
//!
//! Every test name starts `vc_201_060_mastery_` so the vector's evidence is
//! enumerable with `cargo test -p susi-vendor-models vc_201_060`.

use crate::eco_k8s_serving;
use crate::eco_profile;

/// The only spec-like record in the crate cannot carry what the claim
/// requires: `Profile` has no runtime-requirements, secret-reference, or
/// placement-policy fields — nothing to round-trip.
#[test]
fn vc_201_060_mastery_profile_has_no_portability_fields() {
    let p = eco_k8s_serving::profile().expect("bundled k8s-serving profile");
    let json = serde_json::to_string(&p).expect("serializes");
    for field in [
        "runtime_requirements",
        "secret_refs",
        "secret_references",
        "placement",
        "placement_policies",
    ] {
        assert!(
            !json.contains(field),
            "deployment spec cannot express {field} — nothing to round-trip"
        );
    }
}

/// Falsification of "restore": a spec document that *does* carry placement /
/// runtime-requirement / secret-reference fields parses with no error and the
/// fields are silently dropped on re-serialization — no `deny_unknown_fields`,
/// no portability diagnostic. Restore loses the claimed data invisibly.
#[test]
fn vc_201_060_mastery_restore_drops_spec_fields_silently() {
    let p = eco_k8s_serving::profile().expect("bundled k8s-serving profile");
    let mut doc: serde_json::Value = serde_json::to_value(&p).expect("to value");
    let obj = doc.as_object_mut().expect("profile is an object");
    obj.insert(
        "runtime_requirements".into(),
        serde_json::json!({"gpu": "a100-80g", "cuda": ">=12.4"}),
    );
    obj.insert(
        "secret_refs".into(),
        serde_json::json!(["vault://prod/hf-token"]),
    );
    obj.insert(
        "placement".into(),
        serde_json::json!({"region": "eu-central", "residency": "eu-only"}),
    );
    let restored = eco_profile::parse(&serde_json::to_string(&doc).unwrap())
        .expect("parse accepts doc carrying nonportable fields — no error raised");
    let back = serde_json::to_value(&restored).unwrap();
    let back_obj = back.as_object().unwrap();
    assert!(back_obj.get("runtime_requirements").is_none());
    assert!(back_obj.get("secret_refs").is_none());
    assert!(back_obj.get("placement").is_none());
}

/// Falsification of "compatibility checks surface nonportable settings":
/// `validate` checks document shape (ids, auth headers, endpoint paths) — a
/// doc asserting nonportable requirements still validates clean.
#[test]
fn vc_201_060_mastery_validate_ignores_nonportable_settings() {
    let p = eco_k8s_serving::profile().expect("bundled k8s-serving profile");
    let mut doc: serde_json::Value = serde_json::to_value(&p).expect("to value");
    let obj = doc.as_object_mut().expect("profile is an object");
    obj.insert(
        "runtime_requirements".into(),
        serde_json::json!({"rdma": true, "local_nvme": true}),
    );
    obj.insert("secret_refs".into(), serde_json::json!(["k8s://ns/tok"]));
    obj.insert(
        "placement".into(),
        serde_json::json!({"topology": "single-node-only"}),
    );
    let restored = eco_profile::parse(&serde_json::to_string(&doc).unwrap())
        .expect("nonportable settings produce no compatibility finding");
    assert!(eco_profile::validate(&restored).is_empty());
}

/// The closest thing to a restore path — key-source import — imports only
/// credential names, not deployment specs; unrelated to the claim's surface.
/// (Existence check on the real surface: k8s-serving exposes only `profile()`.)
#[test]
fn vc_201_060_mastery_no_export_restore_surface() {
    // `eco_k8s_serving` exposes exactly `profile()` — a knowledge-base load,
    // not an export. There is no `export_spec`/`restore_spec`/`roundtrip`
    // anywhere in the crate; this test documents the module's whole surface
    // and the claim's missing half.
    let _p = eco_k8s_serving::profile().expect("profile loads");
    assert_eq!(eco_k8s_serving::PROFILE_ID, "k8s-serving");
    assert_eq!(eco_k8s_serving::SUBJECT_ID, "kserve");
}

/// Holds: the underlying profile round-trip is faithful for the fields the
/// model actually carries — the refutation is about absent surfaces, not
/// corrupt serialization.
#[test]
fn vc_201_060_mastery_serde_roundtrip_is_faithful_for_modeled_fields() {
    let p = eco_k8s_serving::profile().expect("bundled k8s-serving profile");
    let json = serde_json::to_string(&p).expect("serializes");
    let back = eco_profile::parse(&json).expect("round-trip parses");
    assert_eq!(p, back);
}
