//! Anthropic-compatible `/v1/messages` endpoint translation.
//!
//! Anthropic SDK clients POST Messages requests; the brain speaks
//! OpenAI-chat internally. This module is the bidirectional translator:
//! Messages request → chat request, chat response → Messages response
//! (content blocks, `tool_use`, `stop_reason`, `usage`), plus the SSE
//! event sequence for `stream: true`.

use serde_json::{Value, json};

/// Anthropic Messages request → internal (OpenAI-shaped) chat request.
/// `system` becomes a leading system message; Anthropic content blocks
/// flatten to text; `tools` pass through as function tools.
///
/// # Errors
/// A plain-English message when required fields are missing.
pub fn to_chat_request(req: &Value) -> Result<Value, String> {
    let model = req
        .get("model")
        .and_then(Value::as_str)
        .ok_or("missing required field: model")?;
    let messages = req
        .get("messages")
        .and_then(Value::as_array)
        .ok_or("missing required field: messages")?;
    if req.get("max_tokens").is_none() {
        return Err("missing required field: max_tokens".to_string());
    }

    let mut out_msgs: Vec<Value> = Vec::new();
    if let Some(system) = req.get("system") {
        let text = match system {
            Value::String(s) => s.clone(),
            Value::Array(blocks) => blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => String::new(),
        };
        if !text.is_empty() {
            out_msgs.push(json!({"role": "system", "content": text}));
        }
    }
    for m in messages {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
        let content = match m.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(blocks)) => flatten_blocks(blocks),
            _ => String::new(),
        };
        out_msgs.push(json!({"role": role, "content": content}));
    }

    let mut out = json!({
        "model": model,
        "messages": out_msgs,
        "max_tokens": req["max_tokens"],
        "stream": req.get("stream").and_then(Value::as_bool).unwrap_or(false),
    });
    if let Some(tools) = req.get("tools").and_then(Value::as_array) {
        let fns: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.get("name").and_then(Value::as_str).unwrap_or(""),
                        "description": t.get("description").cloned().unwrap_or(Value::Null),
                        "parameters": t.get("input_schema").cloned().unwrap_or(json!({})),
                    }
                })
            })
            .collect();
        out["tools"] = json!(fns);
    }
    Ok(out)
}

fn flatten_blocks(blocks: &[Value]) -> String {
    let mut parts = Vec::new();
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = b.get("text").and_then(Value::as_str) {
                    parts.push(t.to_string());
                }
            }
            Some("tool_result") => {
                let c = b.get("content").and_then(Value::as_str).unwrap_or_default();
                parts.push(format!("<tool_result>{c}</tool_result>"));
            }
            Some("tool_use") => {
                parts.push(format!(
                    "<tool_use name=\"{}\">{}</tool_use>",
                    b.get("name").and_then(Value::as_str).unwrap_or(""),
                    b.get("input").map_or("{}".to_string(), |v| v.to_string())
                ));
            }
            _ => {}
        }
    }
    parts.join("\n")
}

/// Map an OpenAI finish_reason to Anthropic stop_reason.
#[must_use]
pub fn stop_reason(finish: &str) -> &'static str {
    match finish {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
        _ => "end_turn",
    }
}

/// Chat response → Anthropic Messages response.
#[must_use]
pub fn from_chat_response(resp: &Value, model: &str) -> Value {
    let choice = resp
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .cloned()
        .unwrap_or(Value::Null);
    let msg = choice.get("message").cloned().unwrap_or(Value::Null);

    let mut content: Vec<Value> = Vec::new();
    if let Some(tool_calls) = msg.get("tool_calls").and_then(Value::as_array) {
        for tc in tool_calls {
            let f = tc.get("function").cloned().unwrap_or(Value::Null);
            content.push(json!({
                "type": "tool_use",
                "id": tc.get("id").cloned().unwrap_or(json!("toolu_1")),
                "name": f.get("name").cloned().unwrap_or(Value::Null),
                "input": f
                    .get("arguments")
                    .and_then(Value::as_str)
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .unwrap_or(json!({})),
            }));
        }
    }
    if let Some(text) = msg.get("content").and_then(Value::as_str)
        && !text.is_empty()
    {
        content.insert(0, json!({"type": "text", "text": text}));
    }
    if content.is_empty() {
        content.push(json!({"type": "text", "text": ""}));
    }

    let finish = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .unwrap_or("stop");
    let usage = resp.get("usage").cloned().unwrap_or(json!({}));
    json!({
        "id": resp.get("id").and_then(Value::as_str).unwrap_or("msg_susi"),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason(finish),
        "stop_sequence": null,
        "usage": {
            "input_tokens": usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
            "output_tokens": usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0),
        },
    })
}

/// The SSE event sequence Anthropic streaming clients expect, derived
/// from the final assembled response.
#[must_use]
pub fn stream_events(resp: &Value, model: &str) -> Vec<(String, Value)> {
    let final_msg = from_chat_response(resp, model);
    let text = final_msg["content"]
        .as_array()
        .and_then(|c| {
            c.iter()
                .find(|b| b["type"] == "text")
                .and_then(|b| b["text"].as_str())
        })
        .unwrap_or_default()
        .to_string();
    vec![
        (
            "message_start".to_string(),
            json!({
                "type": "message_start",
                "message": {
                    "id": final_msg["id"],
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [],
                    "usage": {"input_tokens": final_msg["usage"]["input_tokens"], "output_tokens": 0},
                },
            }),
        ),
        (
            "content_block_start".to_string(),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
        (
            "content_block_delta".to_string(),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
        ),
        (
            "content_block_stop".to_string(),
            json!({"type": "content_block_stop", "index": 0}),
        ),
        (
            "message_delta".to_string(),
            json!({
                "type": "message_delta",
                "delta": {"stop_reason": final_msg["stop_reason"], "stop_sequence": null},
                "usage": {"output_tokens": final_msg["usage"]["output_tokens"]},
            }),
        ),
        ("message_stop".to_string(), json!({"type": "message_stop"})),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_gateway_request_translates_system_and_blocks() {
        let req = json!({
            "model": "claude-x",
            "max_tokens": 100,
            "system": "You are terse.",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hi"}]},
                {"role": "assistant", "content": "hello"},
            ],
        });
        let out = to_chat_request(&req).unwrap();
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "You are terse.");
        assert_eq!(msgs[1]["content"], "hi");
        assert_eq!(out["max_tokens"], 100);
    }

    #[test]
    fn anthropic_gateway_request_requires_fields() {
        assert!(to_chat_request(&json!({"messages": []})).is_err());
        assert!(to_chat_request(&json!({"model": "m"})).is_err());
        assert!(to_chat_request(&json!({"model": "m", "messages": []})).is_err());
    }

    #[test]
    fn anthropic_gateway_tools_become_functions() {
        let req = json!({
            "model": "m", "max_tokens": 1,
            "messages": [{"role": "user", "content": "x"}],
            "tools": [{"name": "get_weather", "description": "w", "input_schema": {"type": "object"}}],
        });
        let out = to_chat_request(&req).unwrap();
        assert_eq!(out["tools"][0]["function"]["name"], "get_weather");
    }

    #[test]
    fn anthropic_gateway_response_has_content_blocks() {
        let resp = json!({
            "id": "chatcmpl-1",
            "choices": [{"message": {"role": "assistant", "content": "hello there"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 3},
        });
        let out = from_chat_response(&resp, "m");
        assert_eq!(out["type"], "message");
        assert_eq!(out["content"][0]["type"], "text");
        assert_eq!(out["content"][0]["text"], "hello there");
        assert_eq!(out["stop_reason"], "end_turn");
        assert_eq!(out["usage"]["input_tokens"], 10);
    }

    #[test]
    fn anthropic_gateway_tool_calls_become_tool_use() {
        let resp = json!({
            "choices": [{
                "message": {"tool_calls": [{"id": "call_1", "function": {"name": "f", "arguments": "{\"x\":1}"}}]},
                "finish_reason": "tool_calls",
            }],
        });
        let out = from_chat_response(&resp, "m");
        assert_eq!(out["content"][0]["type"], "tool_use");
        assert_eq!(out["content"][0]["name"], "f");
        assert_eq!(out["content"][0]["input"]["x"], 1);
        assert_eq!(out["stop_reason"], "tool_use");
    }

    #[test]
    fn anthropic_gateway_stream_sequence_is_well_formed() {
        let resp = json!({
            "choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 1},
        });
        let events = stream_events(&resp, "m");
        let names: Vec<&str> = events.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(events[2].1["delta"]["text"], "hi");
        assert_eq!(events[4].1["usage"]["output_tokens"], 1);
    }

    #[test]
    fn anthropic_gateway_stop_reasons_map() {
        assert_eq!(stop_reason("stop"), "end_turn");
        assert_eq!(stop_reason("length"), "max_tokens");
        assert_eq!(stop_reason("tool_calls"), "tool_use");
    }
}
