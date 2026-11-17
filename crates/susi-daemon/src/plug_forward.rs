//! Host-absent forwarding for daemon feature topics served by `susi-plug-host`.

use serde_json::Value;

/// What a caller says when `susi-plug-host` is not reachable for `topic`.
pub fn absent(topic: &str) -> String {
    format!(
        "`{topic}` is served by Susi Commercial Apps: install it and run `susi-plug-host` \
         (or start the commercial plug host so the plane bus can reach it)"
    )
}

/// Rewrite a plane-bus miss so the commercial command is named.
pub fn call(result: Result<Value, String>, topic: &str) -> Result<Value, String> {
    match result {
        Err(e) if e.contains("no handler for topic") || e.contains("unhandled topic") => {
            Err(absent(topic))
        }
        other => other,
    }
}
