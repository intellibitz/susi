use super::*;

fn reply(value: Value, is_error: bool) -> Value {
    json!({"content":[{"type":"text","text":value.to_string()}],"isError":is_error})
}

#[test]
fn successful_native_values_do_not_gain_error_semantics() {
    for value in [
        Value::Null,
        json!(""),
        json!("I/O Error: ordinary data"),
        json!({"error":"data"}),
    ] {
        assert_eq!(
            decode_result::<Value>(reply(value.clone(), false)).unwrap(),
            value
        );
    }
    let clean = FoldOutcome::Clean {
        wrote: vec!["new".into()],
        deleted: vec!["old".into()],
    };
    assert_eq!(
        decode_result::<FoldOutcome>(reply(serde_json::to_value(&clean).unwrap(), false)).unwrap(),
        clean
    );
}

#[test]
fn native_failures_keep_exact_kind_code_retryability_and_display() {
    for kind in ["Governance", "Config", "Io", "Protocol", "Internal"] {
        let original = crate::susi_error::rewrap(kind, "fixture failure".into());
        let payload = json!({"schema":"susi-swarm-storage-error-v1","kind":original.kind_name(),
            "code":original.code(),"retryable":original.retryable(),"message":original.to_string()});
        let error = decode_result::<Value>(reply(payload.clone(), true)).unwrap_err();
        assert_eq!(error.kind_name(), original.kind_name());
        assert_eq!(error.code(), original.code());
        assert_eq!(error.retryable(), original.retryable());
        assert_eq!(error.to_string(), original.to_string());
        for (field, value) in [
            ("schema", json!("wrong")),
            ("kind", json!("Other")),
            ("code", json!("susi.other")),
            ("retryable", json!(!original.retryable())),
        ] {
            let mut invalid = payload.clone();
            invalid[field] = value;
            assert_eq!(
                decode_result::<Value>(reply(invalid, true))
                    .unwrap_err()
                    .kind_name(),
                "Protocol"
            );
        }
    }
}

#[test]
fn malformed_or_ambiguous_wire_results_fail_as_protocol_errors() {
    for response in [
        json!({}),
        json!({"content":[]}),
        json!({"content":[{"type":"text","text":"null"},{"type":"text","text":"null"}]}),
        json!({"content":[{"type":"image","text":"null"}]}),
        json!({"content":[{"type":"text","text":"not-json"}]}),
        json!({"content":[{"type":"text","text":"null"}],"isError":"false"}),
        reply(json!({"message":"untyped"}), true),
    ] {
        assert_eq!(
            decode_result::<Value>(response).unwrap_err().kind_name(),
            "Protocol"
        );
    }
}

#[test]
fn native_scope_paths_and_caller_owned_snapshots_roundtrip_without_loss() {
    let plain = json!({"owner":"worker", "dir":"plain/path", "manifest":{"file":{"len":3,"hash":vec![0;32]}}});
    let scope: WorkerScope = decode_result(reply(plain.clone(), false)).unwrap();
    assert_eq!(serde_json::to_value(scope).unwrap(), plain);
    #[cfg(any(unix, windows))]
    {
        #[cfg(unix)]
        let native = json!({"platform":"unix","units":[112,255]});
        #[cfg(windows)]
        let native = json!({"platform":"windows","units":[112,55296]});
        let mut wire = plain;
        wire["dir"] = native.clone();
        let mut scope: WorkerScope = decode_result(reply(wire.clone(), false)).unwrap();
        assert_eq!(serde_json::to_value(&scope).unwrap(), wire);
        scope.dir = susi_paths::native_path::decode(&native).unwrap();
        assert_eq!(path(&scope.dir).unwrap(), native);
    }
    let state = json!({"base":"workspace","scopes_root":"state","exact_names":true});
    let isolation: WriterIsolation = decode_result(reply(state.clone(), false)).unwrap();
    assert_eq!(serde_json::to_value(isolation).unwrap(), state);
}
