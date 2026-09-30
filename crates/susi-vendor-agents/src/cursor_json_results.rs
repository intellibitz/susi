//! Parse Cursor structured output.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CursorResult {
    pub ok: bool,
    pub summary: String,
    pub files_changed: Vec<String>,
}

/// Extract a structured result from Cursor agent JSON output.
pub fn parse_cursor_json(raw: &str) -> Result<CursorResult, String> {
    let v: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let ok = v.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let summary = v
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let files_changed = v
        .get("files_changed")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(CursorResult {
        ok,
        summary,
        files_changed,
    })
}

#[cfg(test)]
mod cursor_json_results_tests {
    use super::*;

    #[test]
    fn cursor_json_results_parses_structured() {
        let r =
            parse_cursor_json(r#"{"ok":true,"summary":"done","files_changed":["a.rs","b.rs"]}"#)
                .unwrap();
        assert!(r.ok);
        assert_eq!(r.files_changed.len(), 2);
        assert!(parse_cursor_json("not-json").is_err());
    }
}
