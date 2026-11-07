//! A2A v1.0 JSON-RPC `message/send` wire shapes shared by every outbound
//! A2A caller (`a2a_delegate`, external `a2a` peers).
//!
//! v1.0 parts carry their type as the member name (`{"text": …}`, no
//! `kind`), roles are `ROLE_*` enums, and a non-streaming result is either
//! `{"task": Task}` or `{"message": Message}`. Requests declare the revision
//! in the `A2A-Version` header — a v1.0 server treats a missing header as 0.3.

use serde_json::{json, Value};

/// Protocol revision sent as the `A2A-Version` header.
pub const A2A_VERSION: &str = "1.0";

/// JSON-RPC `message/send` request carrying one user text part.
pub fn message_send_request(text: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "message/send",
        "params": {
            "message": {
                "role": "ROLE_USER",
                "parts": [{"text": text}],
                "messageId": format!("susi-{}", crate::susi_config::cluster_key::random_nonce_hex()),
            }
        }
    })
}

/// Validate that a JSON-RPC document conforms to the A2A v1.0 `message/send` request contract.
///
/// # Errors
/// Returns a descriptive error string if any required JSON-RPC envelope or A2A v1.0 message
/// field is missing or malformed.
pub fn verify_message_send_request(doc: &Value) -> Result<(), String> {
    if doc.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err("invalid or missing jsonrpc version (expected \"2.0\")".to_string());
    }
    if doc.get("method").and_then(Value::as_str) != Some("message/send") {
        return Err("invalid or missing method (expected \"message/send\")".to_string());
    }
    if !doc
        .get("id")
        .is_some_and(|v| v.is_number() || v.is_string())
    {
        return Err("invalid or missing id (expected number or string)".to_string());
    }
    if doc.pointer("/params/message/role").and_then(Value::as_str) != Some("ROLE_USER") {
        return Err("invalid or missing /params/message/role (expected \"ROLE_USER\")".to_string());
    }
    if !doc
        .pointer("/params/message/messageId")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
    {
        return Err("invalid or missing /params/message/messageId".to_string());
    }
    let parts = doc
        .pointer("/params/message/parts")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing or non-array /params/message/parts".to_string())?;
    if parts.is_empty() {
        return Err("/params/message/parts must not be empty".to_string());
    }
    if !parts.iter().all(|part| {
        part.get("text")
            .and_then(Value::as_str)
            .is_some_and(|t| !t.is_empty())
    }) {
        return Err(
            "every element in /params/message/parts must carry a non-empty \"text\" string"
                .to_string(),
        );
    }
    Ok(())
}

/// Text parts of a v1.0 `parts` array, newline-joined.
fn parts_text(parts: Option<&Value>) -> String {
    parts
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Fold a `message/send` JSON-RPC response into a one-line summary:
/// `task <state>: <reply>` for a task result (status message, else artifact
/// text, else the raw task), `message: <reply>` for a direct message. A JSON-RPC error, or a
/// result that is neither shape, is `Err`.
pub fn reply_summary(doc: &Value) -> Result<String, String> {
    if let Some(err) = doc.get("error") {
        return Err(format!("rpc error: {err}"));
    }
    let result = doc
        .get("result")
        .ok_or_else(|| "response carries neither result nor error".to_string())?;
    if let Some(task) = result.get("task") {
        let state = task
            .pointer("/status/state")
            .and_then(Value::as_str)
            .map_or_else(
                || "unknown".to_string(),
                |s| s.trim_start_matches("TASK_STATE_").to_ascii_lowercase(),
            );
        let mut reply = parts_text(task.pointer("/status/message/parts"));
        if reply.is_empty() {
            reply = task
                .get("artifacts")
                .and_then(Value::as_array)
                .map(|artifacts| {
                    artifacts
                        .iter()
                        .map(|a| parts_text(a.get("parts")))
                        .filter(|t| !t.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
        }
        return Ok(if reply.is_empty() {
            format!("task {state} (no reply text): {task}")
        } else {
            format!("task {state}: {reply}")
        });
    }
    if let Some(message) = result.get("message") {
        return Ok(format!("message: {}", parts_text(message.get("parts"))));
    }
    Err(format!("result is neither a task nor a message: {result}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_uses_v1_part_and_role_shapes() {
        let req = message_send_request("hi");
        let msg = &req["params"]["message"];
        assert_eq!(msg["role"], "ROLE_USER");
        assert_eq!(msg["parts"], json!([{"text": "hi"}]));
        assert!(msg["messageId"]
            .as_str()
            .is_some_and(|id| id.starts_with("susi-")));
    }

    #[test]
    fn summarizes_task_message_and_error_results() {
        let task = json!({"result": {"task": {"status": {
            "state": "TASK_STATE_COMPLETED",
            "message": {"role": "ROLE_AGENT", "parts": [{"text": "done"}]}
        }}}});
        assert_eq!(reply_summary(&task).unwrap(), "task completed: done");

        let artifact = json!({"result": {"task": {
            "status": {"state": "TASK_STATE_COMPLETED"},
            "artifacts": [{"artifactId": "a", "parts": [{"text": "out"}]}]
        }}});
        assert_eq!(reply_summary(&artifact).unwrap(), "task completed: out");

        let silent =
            json!({"result": {"task": {"id": "t9", "status": {"state": "TASK_STATE_FAILED"}}}});
        let summary = reply_summary(&silent).unwrap();
        assert!(
            summary.starts_with("task failed (no reply text): "),
            "{summary}"
        );
        assert!(summary.contains("t9"), "{summary}");

        let message = json!({"result": {"message": {"parts": [{"text": "hey"}]}}});
        assert_eq!(reply_summary(&message).unwrap(), "message: hey");

        let error = json!({"error": {"code": -32001, "message": "Task not found"}});
        assert!(reply_summary(&error)
            .unwrap_err()
            .contains("Task not found"));
        assert!(reply_summary(&json!({"result": {}})).is_err());
    }

    #[test]
    fn a2a_version_constant_and_multi_part_newline_joining_contract() {
        assert_eq!(A2A_VERSION, "1.0");
        let multi = json!({
            "result": {
                "message": {
                    "parts": [
                        {"text": "line1"},
                        {"other": 1},
                        {"text": "line2"}
                    ]
                }
            }
        });
        assert_eq!(
            reply_summary(&multi).unwrap(),
            "message: line1\nline2".to_string()
        );
    }

    #[test]
    fn reply_summary_rejects_empty_document_and_missing_state_defaults_to_unknown() {
        let err = reply_summary(&json!({})).unwrap_err();
        assert!(err.contains("neither result nor error"), "{err}");

        let task_unknown = json!({
            "result": {
                "task": {
                    "status": {
                        "message": {
                            "parts": [{"text": "ok"}]
                        }
                    }
                }
            }
        });
        assert_eq!(
            reply_summary(&task_unknown).unwrap(),
            "task unknown: ok".to_string()
        );
    }

    #[test]
    fn reply_summary_falls_back_to_multiple_artifacts_joined_by_newline() {
        let reply = json!({
            "result": {
                "task": {
                    "status": {
                        "state": "completed"
                    },
                    "artifacts": [
                        {"parts": [{"text": "first-artifact"}]},
                        {"parts": [{"text": "second-artifact"}]}
                    ]
                }
            }
        });
        assert_eq!(
            reply_summary(&reply).unwrap(),
            "task completed: first-artifact\nsecond-artifact".to_string()
        );
    }

    #[test]
    fn verify_message_send_request_validates_v1_envelope_and_parts() {
        let valid = message_send_request("hello");
        assert_eq!(verify_message_send_request(&valid), Ok(()));

        // String id is also valid in JSON-RPC 2.0
        let mut string_id = valid.clone();
        string_id["id"] = json!("req-42");
        assert_eq!(verify_message_send_request(&string_id), Ok(()));

        // Invalid jsonrpc version
        let mut bad_rpc = valid.clone();
        bad_rpc["jsonrpc"] = json!("1.0");
        assert!(verify_message_send_request(&bad_rpc)
            .unwrap_err()
            .contains("jsonrpc"));

        // Invalid method
        let mut bad_method = valid.clone();
        bad_method["method"] = json!("tasks/get");
        assert!(verify_message_send_request(&bad_method)
            .unwrap_err()
            .contains("method"));

        // Invalid id (null or boolean)
        let mut bad_id = valid.clone();
        bad_id["id"] = Value::Null;
        assert!(verify_message_send_request(&bad_id)
            .unwrap_err()
            .contains("id"));

        // Invalid role
        let mut bad_role = valid.clone();
        bad_role["params"]["message"]["role"] = json!("user");
        assert!(verify_message_send_request(&bad_role)
            .unwrap_err()
            .contains("role"));

        // Blank messageId
        let mut bad_msg_id = valid.clone();
        bad_msg_id["params"]["message"]["messageId"] = json!("   ");
        assert!(verify_message_send_request(&bad_msg_id)
            .unwrap_err()
            .contains("messageId"));

        // Empty parts array
        let mut empty_parts = valid.clone();
        empty_parts["params"]["message"]["parts"] = json!([]);
        assert!(verify_message_send_request(&empty_parts)
            .unwrap_err()
            .contains("must not be empty"));

        // Part with empty text
        let mut empty_text = message_send_request("");
        assert!(verify_message_send_request(&empty_text)
            .unwrap_err()
            .contains("non-empty \"text\""));
        empty_text["params"]["message"]["parts"] = json!([{"other": "value"}]);
        assert!(verify_message_send_request(&empty_text)
            .unwrap_err()
            .contains("non-empty \"text\""));
    }
}
