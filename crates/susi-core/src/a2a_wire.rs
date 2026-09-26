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
}
