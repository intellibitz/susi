//! Normalise tool/function calling across vendors.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedToolCall {
    pub name: String,
    pub arguments: Value,
}

/// Accept OpenAI-style or Anthropic-style tool call fragments.
pub fn normalize_tool_call(raw: &Value) -> Option<NormalizedToolCall> {
    if let Some(name) = raw.get("name").and_then(Value::as_str) {
        let arguments = raw
            .get("arguments")
            .cloned()
            .or_else(|| raw.get("input").cloned())
            .unwrap_or_else(|| json!({}));
        return Some(NormalizedToolCall {
            name: name.into(),
            arguments,
        });
    }
    let fn_obj = raw.get("function")?;
    let name = fn_obj.get("name")?.as_str()?.to_string();
    let arguments = fn_obj
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    Some(NormalizedToolCall { name, arguments })
}

#[cfg(test)]
mod tool_call_normalisation_tests {
    use super::*;

    #[test]
    fn tool_call_normalisation_accepts_vendor_shapes() {
        let openai = json!({"function":{"name":"search","arguments":{"q":"x"}}});
        let a = normalize_tool_call(&openai).unwrap();
        assert_eq!(a.name, "search");
        let anth = json!({"name":"search","input":{"q":"x"}});
        assert_eq!(normalize_tool_call(&anth).unwrap().name, "search");
    }

    #[test]
    fn normalize_tool_call_defaults_missing_arguments_to_empty_object() {
        let top_level = json!({"name": "ping"});
        let normalized_top = normalize_tool_call(&top_level).expect("top-level tool call");
        assert_eq!(normalized_top.name, "ping");
        assert_eq!(normalized_top.arguments, json!({}));

        let nested = json!({"function": {"name": "pong"}});
        let normalized_nested = normalize_tool_call(&nested).expect("nested function tool call");
        assert_eq!(normalized_nested.name, "pong");
        assert_eq!(normalized_nested.arguments, json!({}));
    }

    #[test]
    fn normalize_tool_call_rejects_malformed_or_nameless_fragments() {
        assert!(normalize_tool_call(&json!(null)).is_none());
        assert!(normalize_tool_call(&json!("not-an-object")).is_none());
        assert!(normalize_tool_call(&json!({"arguments": {"a": 1}})).is_none());
        assert!(normalize_tool_call(&json!({"name": 42})).is_none());
        assert!(normalize_tool_call(&json!({"function": {"arguments": {}}})).is_none());
    }

    #[test]
    fn normalized_tool_call_round_trips_through_serde_json() {
        let call = NormalizedToolCall {
            name: "read_file".into(),
            arguments: json!({"path": "Cargo.toml"}),
        };
        let encoded = serde_json::to_string(&call).expect("serialize NormalizedToolCall");
        let decoded: NormalizedToolCall =
            serde_json::from_str(&encoded).expect("deserialize NormalizedToolCall");
        assert_eq!(decoded, call);
    }

    #[test]
    fn normalize_tool_call_prefers_top_level_arguments_over_input() {
        let raw = json!({
            "name": "exec_cmd",
            "arguments": {"cmd": "ls"},
            "input": {"cmd": "ignored"}
        });
        let normalized = normalize_tool_call(&raw).expect("normalize tool call");
        assert_eq!(normalized.name, "exec_cmd");
        assert_eq!(normalized.arguments, json!({"cmd": "ls"}));
    }
}
