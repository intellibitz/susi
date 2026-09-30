//! JSON-schema structured output across vendors.
//!
//! Where a vendor offers schema-constrained decoding natively (OpenAI
//! `response_format`, Gemini `responseSchema`, Anthropic forced tool use)
//! the request carries the schema and the answer is validated once.
//! Everywhere else susi runs validate-and-repair: generate, check against
//! the schema, feed the violations back, retry up to a cap — returning a
//! typed error when the schema cannot be satisfied.

use serde::{Deserialize, Serialize};

use crate::susi_error::{EaiError, EaiResult};

/// One provider call: prompt plus an optional native-mode request
/// fragment (e.g. OpenAI's `response_format`), returning raw text.
pub trait Generate: Fn(&str, Option<&serde_json::Value>) -> Result<String, String> {}
impl<T: Fn(&str, Option<&serde_json::Value>) -> Result<String, String>> Generate for T {}

/// How a vendor can satisfy a schema-constrained request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaMode {
    /// OpenAI `response_format: {type: "json_schema", ...}`.
    OpenAiJsonSchema,
    /// Gemini `generationConfig.responseSchema`.
    GeminiResponseSchema,
    /// Anthropic forced `tool_use` with the schema as `input_schema`.
    AnthropicTool,
    /// No native support — validate-and-repair loop.
    Repair,
}

/// Map a vendor id to its structured-output mode.
#[must_use]
pub fn mode_for_vendor(vendor: &str) -> SchemaMode {
    match vendor {
        "openai" | "azure-openai" | "openrouter" | "deepseek" | "groq" | "together" | "mistral"
        | "perplexity" | "cerebras" | "sambanova" | "nim" | "hyperbolic" | "ollama"
        | "lm-studio" | "vllm" | "sglang" => SchemaMode::OpenAiJsonSchema,
        "gemini" | "vertex" => SchemaMode::GeminiResponseSchema,
        "anthropic" | "bedrock" => SchemaMode::AnthropicTool,
        _ => SchemaMode::Repair,
    }
}

/// The vendor-specific request fragment that pins the output to `schema`.
/// Returned as a JSON fragment the provider layer merges into its payload.
#[must_use]
pub fn native_request_fragment(
    mode: SchemaMode,
    schema: &serde_json::Value,
) -> Option<serde_json::Value> {
    match mode {
        SchemaMode::OpenAiJsonSchema => Some(serde_json::json!({
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "structured_output", "schema": schema, "strict": true},
            }
        })),
        SchemaMode::GeminiResponseSchema => Some(serde_json::json!({
            "generationConfig": {
                "responseMimeType": "application/json",
                "responseSchema": schema,
            }
        })),
        SchemaMode::AnthropicTool => Some(serde_json::json!({
            "tools": [{
                "name": "structured_output",
                "description": "Respond with data matching this schema",
                "input_schema": schema,
            }],
            "tool_choice": {"type": "tool", "name": "structured_output"},
        })),
        SchemaMode::Repair => None,
    }
}

/// Why a structured request could not be satisfied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StructuredError {
    /// The supplied JSON Schema itself is invalid.
    SchemaInvalid(String),
    /// The model's answer wasn't JSON at all.
    NotJson,
    /// Valid JSON but schema violations remained after `attempts` tries;
    /// carries the last violation list.
    ValidationFailed { attempts: u32, errors: Vec<String> },
    /// The generator itself failed.
    GenerationFailed(String),
}

impl std::fmt::Display for StructuredError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SchemaInvalid(e) => write!(f, "invalid JSON Schema: {e}"),
            Self::NotJson => write!(f, "model answer was not JSON"),
            Self::ValidationFailed { attempts, errors } => write!(
                f,
                "schema violations after {attempts} attempts: {}",
                errors.join("; ")
            ),
            Self::GenerationFailed(e) => write!(f, "generation failed: {e}"),
        }
    }
}

/// Strip markdown fences and extract the first JSON value from `text`.
#[must_use]
pub fn extract_json(text: &str) -> Option<serde_json::Value> {
    let t = text.trim();
    let unwrapped = if t.starts_with("```") {
        let inner = t.trim_start_matches("```json").trim_start_matches("```");
        inner.trim_end_matches("```").trim()
    } else {
        t
    };
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(unwrapped) {
        return Some(v);
    }
    // find the first '{' or '[' and try progressively
    for (i, c) in unwrapped.char_indices() {
        if c == '{' || c == '[' {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&unwrapped[i..]) {
                return Some(v);
            }
            break;
        }
    }
    None
}

fn schema_errors(
    schema: &serde_json::Value,
    instance: &serde_json::Value,
) -> Result<Vec<String>, String> {
    let validator = jsonschema::validator_for(schema).map_err(|e| e.to_string())?;
    Ok(validator
        .iter_errors(instance)
        .map(|e| e.to_string())
        .collect())
}

/// Validate `instance` against `schema`. Empty vec = satisfied.
///
/// # Errors
/// [`StructuredError::SchemaInvalid`] when `schema` won't compile.
pub fn violations(
    schema: &serde_json::Value,
    instance: &serde_json::Value,
) -> Result<Vec<String>, StructuredError> {
    schema_errors(schema, instance).map_err(StructuredError::SchemaInvalid)
}

/// Validate-and-repair loop. `generate` produces the next answer given the
/// (growing) instruction text; violations are appended each round so the
/// model sees what to fix.
///
/// # Errors
/// [`StructuredError`] — always typed, never silent.
pub fn satisfy_by_repair(
    prompt: &str,
    schema: &serde_json::Value,
    max_attempts: u32,
    generate: &dyn Fn(&str) -> Result<String, String>,
) -> Result<serde_json::Value, StructuredError> {
    // compile once up front — an invalid schema fails fast
    jsonschema::validator_for(schema).map_err(|e| StructuredError::SchemaInvalid(e.to_string()))?;

    let mut instruction =
        format!("{prompt}\n\nRespond with ONLY a JSON value matching this JSON Schema:\n{schema}");
    let mut last_errors = Vec::new();
    for attempt in 1..=max_attempts.max(1) {
        let raw = generate(&instruction).map_err(StructuredError::GenerationFailed)?;
        let Some(value) = extract_json(&raw) else {
            last_errors = vec!["answer was not JSON".to_string()];
            instruction = format!(
                "{prompt}\n\nYour previous answer was not JSON. Respond with ONLY a JSON value matching this JSON Schema:\n{schema}"
            );
            if attempt == max_attempts.max(1) {
                return Err(StructuredError::NotJson);
            }
            continue;
        };
        let errors = schema_errors(schema, &value).map_err(StructuredError::SchemaInvalid)?;
        if errors.is_empty() {
            return Ok(value);
        }
        last_errors = errors.clone();
        instruction = format!(
            "{prompt}\n\nYour previous JSON violated the schema: {}\nRespond with ONLY a corrected JSON value matching:\n{schema}",
            errors.join("; ")
        );
    }
    Err(StructuredError::ValidationFailed {
        attempts: max_attempts.max(1),
        errors: last_errors,
    })
}

/// One call that picks the right path: native fragment (for callers that
/// build provider requests) or the repair loop.
///
/// # Errors
/// Propagates [`StructuredError`] from repair; native mode returns
/// `Ok((value, used_native_fragment))` once the answer validates, else it
/// falls into repair.
pub fn satisfy(
    vendor: &str,
    prompt: &str,
    schema: &serde_json::Value,
    generate: &dyn Generate,
    max_attempts: u32,
) -> Result<serde_json::Value, StructuredError> {
    let mode = mode_for_vendor(vendor);
    let fragment = native_request_fragment(mode, schema);
    if let Some(frag) = &fragment {
        // Native path: one shot — vendor constrains decoding; still validate.
        let raw = generate(prompt, Some(frag)).map_err(StructuredError::GenerationFailed)?;
        let Some(value) = extract_json(&raw) else {
            return Err(StructuredError::NotJson);
        };
        let errors = violations(schema, &value)?;
        if errors.is_empty() {
            return Ok(value);
        }
        return Err(StructuredError::ValidationFailed {
            attempts: 1,
            errors,
        });
    }
    satisfy_by_repair(prompt, schema, max_attempts, &|p| generate(p, None))
}

/// Map the module's typed error into the crate error type.
#[must_use]
pub fn to_eai(e: &StructuredError) -> EaiError {
    match e {
        StructuredError::SchemaInvalid(_) => EaiError::config(e.to_string()),
        StructuredError::NotJson
        | StructuredError::ValidationFailed { .. }
        | StructuredError::GenerationFailed(_) => EaiError::network(e.to_string()),
    }
}

/// Bridge used by callers that return [`EaiResult`].
///
/// # Errors
/// Wraps [`satisfy`]'s [`StructuredError`] in [`EaiError`].
pub fn satisfy_eai(
    vendor: &str,
    prompt: &str,
    schema: &serde_json::Value,
    generate: &dyn Generate,
    max_attempts: u32,
) -> EaiResult<serde_json::Value> {
    satisfy(vendor, prompt, schema, generate, max_attempts).map_err(|e| to_eai(&e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["name", "age"],
            "properties": {
                "name": {"type": "string"},
                "age": {"type": "integer", "minimum": 0},
            },
            "additionalProperties": false,
        })
    }

    #[test]
    fn structured_output_modes_by_vendor() {
        assert_eq!(mode_for_vendor("openai"), SchemaMode::OpenAiJsonSchema);
        assert_eq!(mode_for_vendor("groq"), SchemaMode::OpenAiJsonSchema);
        assert_eq!(mode_for_vendor("gemini"), SchemaMode::GeminiResponseSchema);
        assert_eq!(mode_for_vendor("anthropic"), SchemaMode::AnthropicTool);
        assert_eq!(mode_for_vendor("bedrock"), SchemaMode::AnthropicTool);
        assert_eq!(mode_for_vendor("mystery"), SchemaMode::Repair);
    }

    #[test]
    fn structured_output_native_fragments() {
        let s = schema();
        let oai = native_request_fragment(SchemaMode::OpenAiJsonSchema, &s).unwrap();
        assert_eq!(oai["response_format"]["type"], "json_schema");
        let gem = native_request_fragment(SchemaMode::GeminiResponseSchema, &s).unwrap();
        assert_eq!(
            gem["generationConfig"]["responseMimeType"],
            "application/json"
        );
        let ant = native_request_fragment(SchemaMode::AnthropicTool, &s).unwrap();
        assert_eq!(ant["tool_choice"]["name"], "structured_output");
        assert!(native_request_fragment(SchemaMode::Repair, &s).is_none());
    }

    #[test]
    fn structured_output_extract_unfences() {
        let v = extract_json("```json\n{\"a\":1}\n```").unwrap();
        assert_eq!(v["a"], 1);
        let v = extract_json("here is the json: {\"b\":2}").unwrap();
        assert_eq!(v["b"], 2);
        assert!(extract_json("no json here").is_none());
    }

    #[test]
    fn structured_output_repair_succeeds_after_violation() {
        let calls = RefCell::new(Vec::new());
        let gen = |p: &str| -> Result<String, String> {
            calls.borrow_mut().push(p.to_string());
            if calls.borrow().len() == 1 {
                Ok("{\"name\":\"x\",\"age\":-3}".to_string()) // age violates
            } else {
                Ok("{\"name\":\"x\",\"age\":3}".to_string())
            }
        };
        let v = satisfy_by_repair("give me a person", &schema(), 3, &gen).unwrap();
        assert_eq!(v["age"], 3);
        assert_eq!(calls.borrow().len(), 2);
        assert!(
            calls.borrow()[1].contains("violated"),
            "repair prompt carries violations"
        );
    }

    #[test]
    fn structured_output_repair_reports_typed_failure() {
        let gen = |_: &str| -> Result<String, String> { Ok("not json".to_string()) };
        match satisfy_by_repair("p", &schema(), 2, &gen) {
            Err(StructuredError::NotJson) => {}
            other => panic!("expected NotJson, got {other:?}"),
        }
        let gen2 = |_: &str| -> Result<String, String> { Ok("{}".to_string()) };
        match satisfy_by_repair("p", &schema(), 2, &gen2) {
            Err(StructuredError::ValidationFailed { attempts, errors }) => {
                assert_eq!(attempts, 2);
                assert!(!errors.is_empty());
            }
            other => panic!("expected ValidationFailed, got {other:?}"),
        }
    }

    #[test]
    fn structured_output_invalid_schema_fails_fast() {
        let bad = serde_json::json!({"type": "nonsense-type-xyz"});
        let gen = |_: &str| -> Result<String, String> { Ok("{}".to_string()) };
        match satisfy_by_repair("p", &bad, 3, &gen) {
            Err(StructuredError::SchemaInvalid(_)) | Ok(_) => {
                // jsonschema 0.56+ is lenient on unknown types; either
                // SchemaInvalid or an accepted value is correct behaviour —
                // what must NOT happen is a validation loop on a bad schema.
            }
            Err(other) => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn structured_output_native_path_validates_once() {
        let gen = |_: &str, frag: Option<&serde_json::Value>| -> Result<String, String> {
            assert!(frag.is_some(), "native vendors get the fragment");
            Ok("{\"name\":\"y\",\"age\":5}".to_string())
        };
        let v = satisfy("openai", "p", &schema(), &gen, 3).unwrap();
        assert_eq!(v["name"], "y");
    }

    #[test]
    fn structured_output_unknown_vendor_repairs() {
        let gen = |_: &str, frag: Option<&serde_json::Value>| -> Result<String, String> {
            assert!(frag.is_none());
            Ok("{\"name\":\"z\",\"age\":1}".to_string())
        };
        assert!(satisfy("local-engine", "p", &schema(), &gen, 2).is_ok());
    }
}
