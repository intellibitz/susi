//! JSON-schema structured output across vendors.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredRequest {
    pub vendor: String,
    pub schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredPayload {
    /// Vendor-specific request body fragment enforcing the schema.
    pub body: Value,
}

/// Build a vendor request fragment that asks for JSON matching `schema`.
#[must_use]
pub fn structured_output(req: &StructuredRequest) -> StructuredPayload {
    let vendor = req.vendor.to_ascii_lowercase();
    let body = match vendor.as_str() {
        "openai" | "azure" | "openrouter" => serde_json::json!({
            "response_format": {
                "type": "json_schema",
                "json_schema": { "name": "susi", "schema": req.schema, "strict": true }
            }
        }),
        "anthropic" => serde_json::json!({
            "output_config": { "format": { "type": "json_schema", "schema": req.schema } }
        }),
        "google" | "gemini" | "vertex" => serde_json::json!({
            "generationConfig": { "responseMimeType": "application/json", "responseSchema": req.schema }
        }),
        _ => serde_json::json!({
            "response_format": { "type": "json_object" },
            "susi_schema": req.schema
        }),
    };
    StructuredPayload { body }
}

#[cfg(test)]
mod structured_output_tests {
    use super::*;

    #[test]
    fn structured_output_maps_vendors() {
        let schema = serde_json::json!({"type":"object","properties":{"ok":{"type":"boolean"}}});
        let o = structured_output(&StructuredRequest {
            vendor: "openai".into(),
            schema: schema.clone(),
        });
        assert_eq!(
            o.body["response_format"]["type"],
            Value::String("json_schema".into())
        );
        let a = structured_output(&StructuredRequest {
            vendor: "anthropic".into(),
            schema,
        });
        assert!(a.body.get("output_config").is_some());
    }
}
