//! Tests for desired-state contract (`vc_201_061_*`).

use crate::desired_state::{
    parse_desired_state, round_trip_desired_state, validate_desired_state, DesiredRef,
    DesiredState, DESIRED_STATE_SCHEMA,
};
use std::collections::BTreeMap;

#[test]
fn vc_201_061_valid_document_passes() {
    let mut state = DesiredState::empty_v1();
    state.models.push(DesiredRef {
        id: "model-a".into(),
        kind: "gguf".into(),
        requires: vec![],
        extra: BTreeMap::new(),
    });
    state.runtimes.push(DesiredRef {
        id: "rt-local".into(),
        kind: "candle".into(),
        requires: vec!["model-a".into()],
        extra: BTreeMap::new(),
    });
    assert!(validate_desired_state(&state).is_ok());
}

#[test]
fn vc_201_061_unknown_requirement_fails_before_mutation() {
    let mut state = DesiredState::empty_v1();
    state.tools.push(DesiredRef {
        id: "tool-x".into(),
        kind: "native".into(),
        requires: vec!["missing-peer".into()],
        extra: BTreeMap::new(),
    });
    let err = validate_desired_state(&state).unwrap_err();
    assert!(err.to_string().contains("missing-peer"));
}

#[test]
fn vc_201_061_unknown_keys_survive_round_trip() {
    let json = r#"{
        "schema_version": "susi.desired_state/v1",
        "models": [{"id": "m1", "kind": "gguf", "custom_flag": true}],
        "future_top_level": {"keep": 1}
    }"#;
    let state = parse_desired_state(json).unwrap();
    assert_eq!(state.schema_version, DESIRED_STATE_SCHEMA);
    assert!(state.extra.contains_key("future_top_level"));
    assert_eq!(
        state.models[0].extra.get("custom_flag"),
        Some(&serde_json::Value::Bool(true))
    );
    let again = round_trip_desired_state(&state).unwrap();
    assert!(again.extra.contains_key("future_top_level"));
    assert_eq!(
        again.models[0].extra.get("custom_flag"),
        Some(&serde_json::Value::Bool(true))
    );
}

#[test]
fn vc_201_061_wrong_schema_version_rejected() {
    let json = r#"{"schema_version":"susi.desired_state/v0","models":[]}"#;
    assert!(parse_desired_state(json).is_err());
}
