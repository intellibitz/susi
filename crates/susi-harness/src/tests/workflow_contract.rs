use super::*;

fn envelope(error: &EaiError) -> Value {
    json!({"schema":ERROR_SCHEMA,"kind":error.kind_name(),"code":error.code(),
        "retryable":error.retryable(),"message":error.to_string()})
}

fn error_result(error: Value) -> Value {
    json!({"content":[{"type":"text","text":error.to_string()}],"isError":true})
}

#[test]
fn typed_errors_retain_category_code_retryability_and_display() {
    for original in [
        EaiError::governance("detail"),
        EaiError::hardware("detail"),
        EaiError::protocol("detail"),
        EaiError::inference("detail"),
        EaiError::sandbox("detail"),
        EaiError::config("detail"),
        EaiError::io("detail"),
        EaiError::network("detail"),
        EaiError::filesystem("detail"),
        EaiError::process("detail"),
        EaiError::authentication("detail"),
        EaiError::authorization("detail"),
        EaiError::internal("detail"),
    ] {
        let decoded =
            decode_workflow_result::<Value>(error_result(envelope(&original))).unwrap_err();
        assert_eq!(decoded.kind_name(), original.kind_name());
        assert_eq!(decoded.code(), original.code());
        assert_eq!(decoded.retryable(), original.retryable());
        assert_eq!(decoded.to_string(), original.to_string());
    }
}

#[test]
fn inconsistent_error_envelopes_fail_closed() {
    for (key, value) in [
        ("schema", json!("unknown")),
        ("kind", json!("unknown")),
        ("code", json!("susi.authorization")),
        ("retryable", json!(true)),
        ("message", json!("detail")),
        ("retryable", json!("false")),
    ] {
        let mut error = envelope(&EaiError::config("detail"));
        error[key] = value;
        assert_eq!(
            decode_workflow_result::<Value>(error_result(error))
                .unwrap_err()
                .kind_name(),
            "Protocol"
        );
    }
}

#[test]
fn structured_and_text_results_preserve_scalar_content() {
    for body in [
        json!({"structuredContent":{"value":"report\n"}}),
        json!({"content":[{"type":"text","text":"\"report\\n\""}]}),
    ] {
        assert_eq!(decode_workflow_result::<String>(body).unwrap(), "report\n");
    }
    for body in [
        Value::Null,
        json!({}),
        json!({"isError":"false","structuredContent":{}}),
        json!({"content":[]}),
        json!({"content":[{"type":"text","text":"invalid"}]}),
        json!({"content":[{"type":"text","text":"{}"},{"type":"text","text":"{}"}]}),
    ] {
        assert!(decode_workflow_result::<Value>(body).is_err());
    }
}

#[test]
fn config_paths_and_original_policy_remain_unmodified_on_the_wire() {
    let config = AutoMergeConfig {
        repo: "./repo/../repo".into(),
        receipt_out: Some("./receipt.json".into()),
        require_task_trailer: false,
        max_attempts: 0,
        ..Default::default()
    };
    let value = serde_json::to_value(&config).unwrap();
    assert_eq!(value["repo"], "./repo/../repo");
    assert_eq!(value["receipt_out"], "./receipt.json");
    assert_eq!(value["require_task_trailer"], false);
    assert_eq!(value["max_attempts"], 0);
    assert_eq!(caller_wire_path(Path::new("")).unwrap(), "");
    assert_eq!(caller_wire_path(Path::new("relative")).unwrap(), "relative");
}

#[test]
fn adopted_object_ids_keep_validation_and_case_normalization() {
    let oid = "A".repeat(40);
    assert_eq!(
        parse_adopted_oids(&[oid.clone(), oid]).unwrap(),
        BTreeSet::from(["a".repeat(40)])
    );
    for invalid in ["a".repeat(39), "g".repeat(40), "-option".into()] {
        assert!(parse_adopted_oids(&[invalid]).is_err());
    }
}
