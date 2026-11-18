//! VC-201-061 mastery: versioned desired-state contract.
//!
//! Exercises the production contract entry points (`parse_desired_state`,
//! `validate_desired_state`, `round_trip_desired_state`) with adversarial
//! documents rather than the cited happy-path tests. The claims under test:
//!   * invalid relationships fail before any mutation can be produced
//!   * unknown preserved keys survive a round-trip

use crate::desired_state::{
    parse_desired_state, round_trip_desired_state, validate_desired_state, DesiredState,
    DESIRED_STATE_SCHEMA,
};
use serde_json::Value;

/// Every invalid-relationship class must be rejected at the parse boundary:
/// `parse_desired_state` is the only constructor callers are given, so an
/// `Err` here is the "fails before mutation" guarantee — no `DesiredState`
/// escapes to mutate anything with.
#[test]
fn vc_201_061_mastery_invalid_relationships_fail_before_mutation() {
    // A dangling `requires` mixed into an otherwise-valid document.
    let dangling = r#"{
        "schema_version": "susi.desired_state/v1",
        "models": [{"id": "m1"}],
        "peers": [{"id": "p1", "requires": ["ghost"]}],
        "tools": [{"id": "t1", "requires": ["m1"]}]
    }"#;
    let err = parse_desired_state(dangling).unwrap_err();
    assert!(err.to_string().contains("ghost"), "{err}");

    // A `requires` that names a policy key is still an unknown entity id.
    let requires_policy_key = r#"{
        "schema_version": "susi.desired_state/v1",
        "policy": {"retention": "30d"},
        "tools": [{"id": "t1", "requires": ["retention"]}]
    }"#;
    assert!(parse_desired_state(requires_policy_key).is_err());

    // Duplicate id across *different* collections (model + provider).
    let cross_collection_dup = r#"{
        "schema_version": "susi.desired_state/v1",
        "models": [{"id": "shared"}],
        "providers": [{"id": "shared"}]
    }"#;
    let err = parse_desired_state(cross_collection_dup).unwrap_err();
    assert!(err.to_string().contains("duplicate"), "{err}");

    // Empty id is an invalid entity.
    let empty_id = r#"{
        "schema_version": "susi.desired_state/v1",
        "runtimes": [{"id": ""}]
    }"#;
    assert!(parse_desired_state(empty_id).is_err());

    // `requires: [""]` cannot be satisfied by any declared id.
    let requires_empty = r#"{
        "schema_version": "susi.desired_state/v1",
        "tools": [{"id": "t1", "requires": [""]}]
    }"#;
    assert!(parse_desired_state(requires_empty).is_err());
}

/// Persisting a desired-state document means parsing it first; when parse
/// fails the target file must be byte-identical afterwards. This mirrors the
/// only production idiom available (`let state = parse_desired_state(..)?`
/// gates the write), proving the invalid document cannot reach the mutation.
#[test]
fn vc_201_061_mastery_failed_parse_leaves_persisted_state_untouched() {
    let dir = std::env::temp_dir().join(format!("vc_201_061_mastery_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("desired_state.json");
    let prior = r#"{"schema_version": "susi.desired_state/v1", "models": [{"id": "m1"}]}"#;
    std::fs::write(&target, prior).unwrap();

    let hostile = r#"{
        "schema_version": "susi.desired_state/v1",
        "models": [{"id": "m1", "requires": ["deleted-model"]}]
    }"#;
    // The apply pattern: parse, and only on success persist.
    if let Ok(state) = parse_desired_state(hostile) {
        crate::atomic_write_json_pretty(&target, &state).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        prior,
        "invalid document must not have been persisted"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Unknown keys must survive at top level, inside entity entries and inside
/// `policy`, across a *stable* double round-trip: parse → serialize → parse →
/// serialize must produce identical JSON, proving nothing is dropped or
/// re-labeled on the second pass either.
#[test]
fn vc_201_061_mastery_unknown_keys_survive_stable_round_trip() {
    let json = r#"{
        "schema_version": "susi.desired_state/v1",
        "models": [{"id": "m1", "kind": "gguf", "vendor_extra": {"nested": [1, 2, {"deep": null}]}}],
        "policy": {"retention": "30d", "future_policy_key": {"x": true}},
        "future_top_level": {"keep": [{"a": 1}], "n": 3.5, "s": "txt", "flag": false}
    }"#;
    let first = parse_desired_state(json).unwrap();
    let second = round_trip_desired_state(&first).unwrap();
    // Equal states: no key was dropped or re-typed.
    assert_eq!(first, second);

    let first_json = serde_json::to_string(&first).unwrap();
    let second_json = serde_json::to_string(&second).unwrap();
    assert_eq!(first_json, second_json, "serialization must be stable");

    // Spot-check deep preservation through the flattened `extra` maps.
    let v: Value = serde_json::from_str(&first_json).unwrap();
    assert_eq!(
        v["models"][0]["vendor_extra"]["nested"][2]["deep"],
        Value::Null
    );
    assert_eq!(v["future_top_level"]["n"], serde_json::json!(3.5));
    assert_eq!(
        v["policy"]["future_policy_key"]["x"],
        serde_json::json!(true)
    );
}

/// The schema version is explicit and checked: missing, wrong, or non-string
/// versions are all rejected before a state is produced; a correct version
/// with only unknown keys still parses.
#[test]
fn vc_201_061_mastery_schema_version_is_explicit_and_checked() {
    assert!(parse_desired_state(r#"{"models": []}"#).is_err());
    assert!(parse_desired_state(r#"{"schema_version": "susi.desired_state/v0"}"#).is_err());
    assert!(parse_desired_state(r#"{"schema_version": 1}"#).is_err());

    let minimal = parse_desired_state(&format!(
        r#"{{"schema_version": "{DESIRED_STATE_SCHEMA}", "unknown_only": [true]}}"#
    ))
    .unwrap();
    assert!(minimal.extra.contains_key("unknown_only"));

    // A hand-built state with a forged version fails validation too —
    // validation is a property of the value, not of the parse path.
    let mut forged = DesiredState::empty_v1();
    forged.schema_version = "susi.desired_state/v999".to_string();
    assert!(validate_desired_state(&forged).is_err());
    assert!(round_trip_desired_state(&forged).is_err());
}

/// Forward references are valid: `requires` may name an id declared later in
/// the same or another collection — validation sees the whole document.
#[test]
fn vc_201_061_mastery_forward_references_resolve() {
    let json = r#"{
        "schema_version": "susi.desired_state/v1",
        "runtimes": [{"id": "rt", "requires": ["m-later", "p-peer"]}],
        "models": [{"id": "m-later"}],
        "peers": [{"id": "p-peer"}]
    }"#;
    assert!(parse_desired_state(json).is_ok());
}
