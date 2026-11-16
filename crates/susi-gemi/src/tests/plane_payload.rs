use super::*;

#[test]
fn malformed_required_fields_keep_exact_topic_errors_before_side_effects() {
    let handler = GemiPlaneHandler;
    for (topic, key, expected) in [
        (topics::GEMI_MODELS_PATH, "model_id", "model_id required"),
        (topics::GEMI_MODELS_INSTALL, "target", "target required"),
        (topics::GEMI_CODING_PREFER, "id", "id is required"),
        (topics::GEMI_ECOSYSTEM_START, "id", "id is required"),
        (topics::GEMI_ECOSYSTEM_EXPLAIN, "id", "id is required"),
        (
            topics::GEMI_ROUTING_CLEAR_COOLDOWN,
            "provider",
            "provider is required",
        ),
    ] {
        for value in [Value::Null, json!(false), json!(12), json!([]), json!({})] {
            assert_eq!(
                handler.handle(topic, json!({key: value})).unwrap_err(),
                expected
            );
        }
        for payload in [Value::Null, json!(false), json!(12), json!([]), json!({})] {
            assert_eq!(handler.handle(topic, payload).unwrap_err(), expected);
        }
    }
}

#[test]
fn payload_strings_remain_untrimmed_and_missing_paths_keep_dot_default() {
    for value in [Value::Null, json!(false), json!(12), json!([]), json!({})] {
        let payload = json!({"workspace": value, "global_dir": value});
        assert_eq!(workspace_path(&payload), PathBuf::from("."));
        assert_eq!(path_field(&payload, "global_dir"), PathBuf::from("."));
    }
    for raw in ["", " ", "./a b/../c", "ümlaut"] {
        let payload = json!({"workspace": raw, "global_dir": raw, "prompt": raw});
        assert_eq!(workspace_path(&payload), PathBuf::from(raw));
        assert_eq!(path_field(&payload, "global_dir"), PathBuf::from(raw));
        assert_eq!(string_field(&payload, "prompt"), Some(raw));
    }
    // The path lookup permits an explicitly empty model ID; validation on
    // the other required fields must not turn into a new global trim rule.
    assert_eq!(
        GemiPlaneHandler
            .handle(topics::GEMI_MODELS_PATH, json!({"model_id": ""}))
            .unwrap(),
        json!({"path": null})
    );
}
