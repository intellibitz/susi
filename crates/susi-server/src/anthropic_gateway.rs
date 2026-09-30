//! Anthropic-compatible `/v1/messages` gateway surface.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnthropicMessagesRequest {
    pub model: String,
    pub messages: Vec<Value>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnthropicMessagesResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub role: String,
    pub content: Vec<Value>,
    pub model: String,
}

/// Map an Anthropic `/v1/messages` body into the internal chat shape.
#[must_use]
pub fn from_anthropic(req: &AnthropicMessagesRequest) -> Value {
    serde_json::json!({
        "model": req.model,
        "messages": req.messages,
        "max_tokens": req.max_tokens.unwrap_or(1024),
        "protocol": "anthropic_messages"
    })
}

/// Build an Anthropic-shaped response from plain assistant text.
#[must_use]
pub fn to_anthropic(model: &str, text: &str, id: &str) -> AnthropicMessagesResponse {
    AnthropicMessagesResponse {
        id: id.into(),
        kind: "message".into(),
        role: "assistant".into(),
        content: vec![serde_json::json!({"type":"text","text": text})],
        model: model.into(),
    }
}

#[cfg(test)]
mod anthropic_gateway_tests {
    use super::*;

    #[test]
    fn anthropic_gateway_roundtrips_messages() {
        let req = AnthropicMessagesRequest {
            model: "claude-3".into(),
            messages: vec![serde_json::json!({"role":"user","content":"hi"})],
            max_tokens: Some(64),
        };
        let internal = from_anthropic(&req);
        assert_eq!(internal["protocol"], "anthropic_messages");
        assert_eq!(internal["max_tokens"], 64);
        let resp = to_anthropic("claude-3", "hello", "msg_1");
        assert_eq!(resp.role, "assistant");
        assert_eq!(resp.content[0]["text"], "hello");
    }
}
