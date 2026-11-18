//! Wire and external stream contracts retained by the native ACP package.
use super::*;
use std::io::Cursor;

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

#[test]
fn external_sse_decodes_crlf_multiline_comments_and_final_unterminated_frame() {
    let input = b": keepalive\r\n\r\nevent: acp.turn.event\r\ndata: {\"delta\":\r\ndata: \"hello\"}\r\n\r\ndata: {\"stop_reason\":\"end_turn\"}";
    let mut frames = Vec::new();
    read_sse_stream(Cursor::new(input), &mut |frame| {
        frames.push(frame);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        frames,
        vec![
            ("acp.turn.event".to_owned(), json!({"delta":"hello"})),
            ("message".to_owned(), json!({"stop_reason":"end_turn"}))
        ]
    );
}

#[test]
fn external_sse_preserves_backpressure_and_malformed_frame_refusal() {
    let mut observed = 0;
    let error = read_sse_stream(Cursor::new(b"data: {}\n\ndata: {}\n\n"), &mut |_| {
        observed += 1;
        Err("consumer cancelled".to_owned())
    })
    .unwrap_err();
    assert_eq!(error, "consumer cancelled");
    assert_eq!(observed, 1);
    let error = read_sse_stream(Cursor::new(b"data: invalid\n\n"), &mut |_| Ok(())).unwrap_err();
    assert!(error.starts_with("malformed SSE JSON frame:"));
}

#[test]
fn external_sse_refuses_oversize_payload_before_delivering_it() {
    let input = format!("data: \"{}\"\n\n", "x".repeat(MAX_HTTP_BODY_BYTES));
    let mut delivered = false;
    let error = read_sse_stream(Cursor::new(input), &mut |_| {
        delivered = true;
        Ok(())
    })
    .unwrap_err();
    assert_eq!(error, "ACP SSE response exceeds configured limit");
    assert!(!delivered);
}
