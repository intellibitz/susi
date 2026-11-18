//! Native types remain pinned to the original server's checked-in wire corpus.
use crate::protocol::*;
use serde_json::{json, Value};

fn golden(name: &str) -> String {
    let corpus: Value = serde_json::from_str(include_str!("golden/a2a_wire.json")).unwrap();
    corpus[name]["body"].as_str().unwrap().to_owned()
}

#[test]
fn completed_task_keeps_original_golden_member_order_and_spellings() {
    let mut message = Message::agent(vec![Part::text("answer to ping")]);
    message.id = "$MSG1".to_owned();
    let task = Task {
        id: "$TASK1".to_owned(),
        context_id: "$CTX1".to_owned(),
        status: TaskStatus {
            state: TaskState::Completed,
            message: Some(message),
            timestamp: Some("<rfc3339>".to_owned()),
        },
    };
    let task_json = serde_json::to_string(&task).unwrap();
    let envelope = format!("{{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{{\"task\":{task_json}}}}}");
    assert_eq!(envelope, golden("message_send"));
}

#[test]
fn push_configuration_keeps_original_golden_fields_and_authentication() {
    let config = TaskPushNotificationConfig {
        tenant: Some("tenant-a".to_owned()),
        id: Some("cfg-2".to_owned()),
        task_id: Some("$TASK1".to_owned()),
        url: "https://127.0.0.1:9/b".to_owned(),
        token: None,
        authentication: Some(AuthenticationInfo {
            scheme: "Basic".to_owned(),
            credentials: Some("Zm9vOmJhcg==".to_owned()),
        }),
    };
    let encoded = serde_json::to_string(&config).unwrap();
    assert_eq!(
        format!("{{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{encoded}}}"),
        golden("create_push_config_with_id")
    );
    assert_eq!(
        serde_json::from_str::<TaskPushNotificationConfig>(&encoded).unwrap(),
        config
    );
}

#[test]
fn invalid_wire_requests_keep_original_golden_error_text() {
    for (name, message) in [
        (
            "send_bad_role",
            json!({"messageId":"m","role":"user","parts":[]}),
        ),
        (
            "send_part_without_content",
            json!({"messageId":"m","role":"ROLE_USER","parts":[{}]}),
        ),
        (
            "send_raw_bad_padding",
            json!({"messageId":"m","role":"ROLE_USER","parts":[{"raw":"aGVsbG8"}]}),
        ),
    ] {
        let error =
            serde_json::from_value::<SendMessageRequest>(json!({"message":message})).unwrap_err();
        let expected: Value = serde_json::from_str(&golden(name)).unwrap();
        assert_eq!(
            format!("Invalid params: {error}"),
            expected["error"]["message"].as_str().unwrap()
        );
        assert_eq!(expected["error"]["code"], error::INVALID_PARAMS);
    }
}

#[test]
fn domain_errors_keep_public_codes_and_messages() {
    for (error, code, message) in [
        (
            A2AError::TaskNotFound("missing".to_owned()),
            -32001,
            "task not found: missing",
        ),
        (
            A2AError::TaskNotCancelable("done".to_owned()),
            -32002,
            "task cannot be canceled: done",
        ),
        (
            A2AError::PushNotificationNotSupported,
            -32003,
            "push notification not supported",
        ),
        (
            A2AError::ExtendedCardNotConfigured,
            -32007,
            "extended card not configured",
        ),
        (A2AError::Other("failed".to_owned()), -32603, "failed"),
    ] {
        assert_eq!(error.code(), code);
        assert_eq!(error.to_string(), message);
    }
}
