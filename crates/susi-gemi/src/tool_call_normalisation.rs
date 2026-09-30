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
}
