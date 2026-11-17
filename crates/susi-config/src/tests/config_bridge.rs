use super::*;
use crate::mcp_client::Transport;
use std::collections::VecDeque;

struct Script {
    replies: VecDeque<Value>,
    requests: Vec<Value>,
}
impl Transport for Script {
    fn exchange(&mut self, request: &Value) -> EaiResult<Value> {
        self.requests.push(request.clone());
        self.replies
            .pop_front()
            .ok_or_else(|| EaiError::io("closed fixture"))
    }
}
fn client(catalog: Value, answer: Value) -> McpClient<Script> {
    McpClient::new(Script {
        replies: VecDeque::from([
            json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25"}}),
            json!({"jsonrpc":"2.0","id":2,"result":{"tools":catalog}}),
            json!({"jsonrpc":"2.0","id":3,"result":answer}),
        ]),
        requests: Vec::new(),
    })
}
fn catalog() -> Value {
    json!([{"name":TOOL,"inputSchema":{"type":"object"}}])
}

#[test]
fn preserves_reply_absence_and_typed_errors_without_replay() {
    for expected in [
        None,
        Some(Value::Null),
        Some(json!({"isError":true,"structuredContent":{"code":"original"}})),
    ] {
        let mut peer = client(catalog(), result(Ok(expected.clone())));
        assert_eq!(
            dispatch(
                &mut peer,
                crate::workflow_wire::WORKFLOW_TOOL,
                &json!({}),
                None
            )
            .unwrap(),
            expected
        );
        assert_eq!(peer.transport().requests.len(), 3);
    }
    let expected = EaiError::authorization("original local permission refusal");
    let mut peer = client(
        catalog(),
        result(Err(EaiError::authorization(
            "original local permission refusal",
        ))),
    );
    let error = dispatch(
        &mut peer,
        crate::workflow_wire::WORKFLOW_TOOL,
        &json!({}),
        None,
    )
    .unwrap_err();
    assert_eq!(error.kind_name(), expected.kind_name());
    assert_eq!(error.code(), expected.code());
    assert_eq!(error.retryable(), expected.retryable());
    assert_eq!(error.to_string(), expected.to_string());
    assert_eq!(peer.transport().requests.len(), 3);
}

#[test]
fn offline_empty_registry_works_and_configured_missing_owner_refuses() {
    let _guard = crate::env_test_lock();
    let home = std::env::temp_dir().join(format!(
        "susi-config-bridge-{}-{}",
        std::process::id(),
        crate::random_nonce_hex()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("HOME", &home)
        .set("XDG_CONFIG_HOME", home.join(".config"))
        .set("XDG_DATA_HOME", home.join(".local/share"))
        .set("XDG_CACHE_HOME", home.join(".cache"))
        .set("SUSI_COMMERCIAL_BIN", home.join("missing-owner"));
    let arguments = json!({"operation":"storage_audit"});
    assert_eq!(
        try_call(crate::workflow_wire::WORKFLOW_TOOL, &arguments, None).unwrap(),
        None
    );
    let registry = crate::mcp::mcp_config_path();
    std::fs::write(&registry, r#"{"mcp_servers":{}}"#).unwrap();
    assert_eq!(
        try_call(crate::workflow_wire::WORKFLOW_TOOL, &arguments, None).unwrap(),
        None
    );
    std::fs::write(
        &registry,
        r#"{"mcp_servers":{"configured":{"command":"provider","args":[],"env":null}}}"#,
    )
    .unwrap();
    assert!(
        try_call(crate::workflow_wire::WORKFLOW_TOOL, &arguments, None)
            .unwrap_err()
            .to_string()
            .contains("config-mcp-bridge")
    );
    std::fs::write(&registry, b"torn registry").unwrap();
    assert!(
        try_call(crate::workflow_wire::WORKFLOW_TOOL, &arguments, None)
            .unwrap_err()
            .to_string()
            .contains("ERROR_CONFIG_DAMAGED")
    );
    drop(env);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn missing_ambiguous_and_malformed_owners_never_prove_absence() {
    for tools in [
        json!([]),
        json!([{"name":TOOL},{"name":TOOL}]),
        json!([{"description":"unnamed"}]),
    ] {
        let mut peer = client(tools, result(Ok(None)));
        assert!(dispatch(
            &mut peer,
            crate::workflow_wire::WORKFLOW_TOOL,
            &json!({}),
            None
        )
        .is_err());
        assert_eq!(peer.transport().requests.len(), 2);
    }
    let mut peer = client(catalog(), json!({"isError":false,"structuredContent":[]}));
    assert!(dispatch(
        &mut peer,
        crate::workflow_wire::WORKFLOW_TOOL,
        &json!({}),
        None
    )
    .is_err());
    assert_eq!(peer.transport().requests.len(), 3);
}

#[test]
fn lossless_caller_path_survives_the_mcp_request() {
    #[cfg(unix)]
    let wire = json!({"platform":"unix","units":[46,47,255]});
    #[cfg(windows)]
    let wire = json!({"platform":"windows","units":[46,92,55296]});
    #[cfg(not(any(unix, windows)))]
    let wire = json!("./caller");
    let path = susi_paths::native_path::decode(&wire).unwrap();
    let mut peer = client(catalog(), result(Ok(None)));
    dispatch(
        &mut peer,
        crate::workflow_wire::WORKFLOW_TOOL,
        &json!({"operation":"storage_audit"}),
        Some(&path),
    )
    .unwrap();
    let request = &peer.transport().requests[2]["params"]["arguments"];
    assert_eq!(request["cwd"], wire);
    assert_eq!(
        serde_json::from_value::<Request>(request.clone())
            .unwrap()
            .cwd,
        Some(path)
    );
}
