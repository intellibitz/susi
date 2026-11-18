//! Shared decoding of the daemon's JSON operator requests.

use serde_json::Value;
use std::path::PathBuf;
use susi_config::JsonFields as _;

pub(super) fn msg(error: impl std::fmt::Display) -> String {
    error.to_string()
}

pub(super) fn field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload
        .field_str(key)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

pub(super) fn number(payload: &Value, key: &str) -> Option<u64> {
    payload.field_u64(key)
}

/// The socket caller has no working directory for relative paths.
pub(super) fn absolute(payload: &Value, key: &str) -> Result<Option<PathBuf>, String> {
    let Some(text) = field(payload, key) else {
        return Ok(None);
    };
    let path = PathBuf::from(text);
    if !path.is_absolute() {
        return Err(format!(
            "{key} must be an absolute path (the socket has no caller working directory)"
        ));
    }
    Ok(Some(path))
}
