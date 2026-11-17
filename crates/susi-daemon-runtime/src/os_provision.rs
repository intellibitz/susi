//! OS provision callers forward to `daemon.os.provision` on `susi-plug-host`.
//!
//! `src/cli/os_cli.rs` still calls these library functions; the
//! implementations live in Susi Commercial Apps.

use serde_json::{Value, json};
use susi_core::plane_bus::{daemon, topics};

fn absent(topic: &str) -> String {
    format!(
        "`{topic}` is served by Susi Commercial Apps: install it and run `susi-plug-host` \
         (or start the commercial plug host so the plane bus can reach it)"
    )
}

fn call_plug(result: Result<Value, String>) -> Result<Value, String> {
    match result {
        Err(e) if e.contains("no handler for topic") || e.contains("unhandled topic") => {
            Err(absent(topics::DAEMON_OS_PROVISION))
        }
        other => other,
    }
}

/// `susi os provision probe --json`: which cloud CLIs this host has.
pub fn probe_report() -> Value {
    call_plug(daemon::os_provision(json!({ "action": "probe" })))
        .unwrap_or_else(|e| json!({ "error": e }))
}

/// `susi os provision list <kind> --json`: the nodes the cloud CLI lists.
pub fn list_report(kind: &str) -> Result<Value, String> {
    call_plug(daemon::os_provision(
        json!({ "action": "list", "kind": kind }),
    ))
}

/// `susi os provision apply <kind> <manifest>`: apply manifest text through
/// the plug host; returns the tool's output. The plane contract reads an
/// absolute path, so the text is staged to a temporary file for the hop.
pub fn apply(kind: &str, manifest_text: &str) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!(
        "susi-os-provision-{}-{}.yaml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, manifest_text).map_err(|e| format!("write {}: {e}", path.display()))?;
    let result = call_plug(daemon::os_provision(json!({
        "action": "apply",
        "kind": kind,
        "manifest": path.display().to_string(),
    })));
    let _ = std::fs::remove_file(&path);
    let value = result?;
    if let Some(output) = value.get("output").and_then(Value::as_str) {
        Ok(output.to_owned())
    } else if let Some(error) = value.get("error").and_then(Value::as_str) {
        Err(error.to_owned())
    } else {
        Ok(value.to_string())
    }
}

/// Dispatch one `daemon.os.provision` request. Kept so leftover in-tree
/// callers that used the old module still compile; the work is on the plug.
pub fn dispatch(payload: &Value) -> Result<Value, String> {
    call_plug(daemon::os_provision(payload.clone()))
}
