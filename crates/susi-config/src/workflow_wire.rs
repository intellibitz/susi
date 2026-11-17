//! The workflow MCP wire (`workflow_harness_v1`) the harness and the storage guard share.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use susi_error::{EaiError, EaiResult};

pub const ERROR_SCHEMA: &str = "susi-workflow-error-v1";

pub const WORKFLOW_TOOL: &str = "workflow_harness_v1";

pub fn workflow_request(
    operation: &str,
    config: &impl Serialize,
) -> EaiResult<(Value, Option<&'static Path>)> {
    let config = serde_json::to_value(config)
        .map_err(|error| EaiError::protocol(format!("serialize workflow request: {error}")))?;
    Ok((
        json!({"operation":operation,"config":config}),
        (operation != "score_task").then_some(Path::new(".")),
    ))
}

/// The fallback runs only after `call` proves (`None`) that no provider claimed the tool.
pub fn workflow_call_or_with<T: DeserializeOwned>(
    operation: &str,
    config: &impl Serialize,
    call: impl FnOnce(&str, &Value, Option<&Path>) -> EaiResult<Option<Value>>,
    fallback: impl FnOnce() -> EaiResult<T>,
) -> EaiResult<T> {
    let (request, cwd) = workflow_request(operation, config)?;
    match call(WORKFLOW_TOOL, &request, cwd)? {
        Some(result) => decode_workflow_result(result),
        None => fallback(),
    }
}

// Relative paths and their diagnostic labels keep the caller's native cwd.
pub fn wire_path(path: &Path) -> EaiResult<Value> {
    susi_paths::native_path::encode(path).map_err(EaiError::config)
}

fn invalid_result() -> EaiError {
    EaiError::protocol("invalid workflow MCP result")
}

#[derive(Deserialize)]
struct WorkflowError {
    schema: String,
    kind: String,
    code: String,
    retryable: bool,
    message: String,
}

fn json_content(result: &Value) -> EaiResult<Value> {
    let content = result
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(invalid_result)?;
    if content.len() != 1 || content[0].get("type").and_then(Value::as_str) != Some("text") {
        return Err(invalid_result());
    }
    let text = content[0]
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(invalid_result)?;
    serde_json::from_str(text).map_err(|_| invalid_result())
}

pub fn decode_workflow_result<T: DeserializeOwned>(result: Value) -> EaiResult<T> {
    if !result.is_object() {
        return Err(invalid_result());
    }
    let is_error = match result.get("isError") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err(invalid_result()),
    };
    let (payload, structured) = match result.get("structuredContent") {
        Some(value) if value.is_object() => (value.clone(), true),
        Some(_) => return Err(invalid_result()),
        None => (json_content(&result)?, false),
    };
    if is_error {
        let wire: WorkflowError = serde_json::from_value(payload).map_err(|_| invalid_result())?;
        if wire.schema != ERROR_SCHEMA {
            return Err(invalid_result());
        }
        let error = susi_error::rewrap(&wire.kind, wire.message.clone());
        if error.kind_name() != wire.kind
            || error.code() != wire.code
            || error.retryable() != wire.retryable
            || error.to_string() != wire.message
        {
            return Err(invalid_result());
        }
        return Err(error);
    }
    match serde_json::from_value(payload.clone()) {
        Ok(value) => Ok(value),
        Err(_)
            if structured
                && payload
                    .as_object()
                    .is_some_and(|object| object.len() == 1 && object.contains_key("value")) =>
        {
            serde_json::from_value(payload["value"].clone()).map_err(|_| invalid_result())
        }
        Err(_) => Err(invalid_result()),
    }
}

pub fn workflow_error_value(error: EaiError) -> Value {
    json!({"schema":ERROR_SCHEMA, "kind":error.kind_name(), "code":error.code(),
        "retryable":error.retryable(), "message":error.to_string()})
}
