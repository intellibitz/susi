//! Wire contracts retained by the native ACP package.
use super::*;
use serde_json::json;

#[test]
fn legacy_roles_defaults_and_refusals_remain_compatible() {
    for (name, role) in [
        (" developer ", AcpMessageRole::System),
        ("MODEL", AcpMessageRole::Assistant),
        ("user", AcpMessageRole::User),
        ("tool", AcpMessageRole::Tool),
    ] {
        assert_eq!(AcpMessageRole::from_wire_str(name).unwrap(), role);
        assert_eq!(serde_json::to_value(role).unwrap(), role.as_str());
    }
    assert_eq!(
        AcpMessageRole::from_wire_str("operator").unwrap_err(),
        (
            JSONRPC_INVALID_PARAMS,
            "Invalid params: unsupported ACP message role 'operator'".to_owned()
        )
    );
    let capabilities: AcpCapabilities = serde_json::from_value(json!({"sessions":true,"streaming":true,"cancellation":true,"capability_admission":true,"mcp_bridge":true})).unwrap();
    assert!(capabilities.session_history);
    let result: AcpTurnResult = serde_json::from_value(json!({"session_id":"s1","turn_id":"t1","stop_reason":"cancelled","output":[],"events":[],"receipt_sha256":"receipt"})).unwrap();
    assert!(result.messages.is_empty());
    assert!(result.inference_messages.is_empty());
    assert!(result.tool_calls.is_empty());
    assert_eq!(result.history_bounds, AcpHistoryBounds::default());
    assert_eq!(result.stop_reason, AcpStopReason::Cancelled);
}

#[test]
fn official_tool_events_keep_member_names_and_omissions() {
    let mut call = AcpToolCall::new("tool-1", "Read workspace");
    assert_eq!(
        serde_json::to_string(&call).unwrap(),
        r#"{"toolCallId":"tool-1","title":"Read workspace","status":"pending"}"#
    );
    call.name = Some("read_file".to_owned());
    call.status = AcpToolCallStatus::InProgress;
    let encoded = serde_json::to_value(&call).unwrap();
    assert_eq!(encoded["name"], "read_file");
    assert_eq!(encoded["status"], "in_progress");
    assert_eq!(
        serde_json::from_value::<AcpToolCall>(encoded).unwrap(),
        call
    );
    let mut update = AcpToolCallUpdate::new("tool-1");
    assert_eq!(
        serde_json::to_value(&update).unwrap(),
        json!({"toolCallId":"tool-1"})
    );
    update.status = Some(AcpToolCallStatus::Failed);
    update.content = Some(vec![AcpToolCallContent::text("cancelled")]);
    assert_eq!(
        serde_json::to_value(&update).unwrap(),
        json!({"toolCallId":"tool-1","status":"failed","content":[{"type":"content","content":{"type":"text","text":"cancelled"}}]})
    );
}
