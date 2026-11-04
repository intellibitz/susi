//! Explicit cloud provisioning: what `susi os provision probe|list|apply`
//! does and the MCP `os_provision` tool does over the `daemon.os.provision`
//! plane topic (zero-UI plan B4 group 9). One implementation for both.
//!
//! Nothing here runs on a tick: `probe` only asks each cloud CLI for its
//! version, and `list` / `apply` run only on an operator's explicit request.

use serde_json::{Value, json};

/// `susi os provision probe --json`: which cloud CLIs this host has.
pub fn probe_report() -> Value {
    let inv = susi_vendor_cloud::probe_all();
    json!({
        "tick_contract": "explicit_probe_only",
        "inventory": inv.iter().map(|c| json!({
            "kind": c.kind,
            "tool": c.tool,
            "available": c.available,
            "summary": c.summary,
        })).collect::<Vec<_>>(),
    })
}

/// `susi os provision list <kind> --json`: the nodes the cloud CLI lists.
pub fn list_report(kind: &str) -> Result<Value, String> {
    let kind = susi_vendor_cloud::CloudKind::parse(kind)?;
    let nodes = susi_vendor_cloud::list_nodes(kind)?;
    Ok(json!({
        "kind": kind.as_str(),
        "explicit": true,
        "nodes": nodes,
    }))
}

/// `susi os provision apply <kind> <manifest>`: apply manifest text through
/// the cloud CLI; returns the tool's output.
pub fn apply(kind: &str, manifest_text: &str) -> Result<String, String> {
    let kind = susi_vendor_cloud::CloudKind::parse(kind)?;
    susi_vendor_cloud::apply_manifest(kind, manifest_text)
}

/// Dispatch one `daemon.os.provision` request: `{action, kind, manifest}`,
/// `manifest` an absolute path read here (the socket has no caller cwd).
pub fn dispatch(payload: &Value) -> Result<Value, String> {
    let field = |key: &str| payload.get(key).and_then(|v| v.as_str()).map(str::trim);
    match field("action").unwrap_or("probe") {
        "probe" => Ok(probe_report()),
        "list" => list_report(field("kind").ok_or("list requires kind")?),
        "apply" => {
            let kind = field("kind").ok_or("apply requires kind")?;
            let manifest = std::path::PathBuf::from(
                field("manifest")
                    .filter(|m| !m.is_empty())
                    .ok_or("apply requires manifest")?,
            );
            if !manifest.is_absolute() {
                return Err("manifest must be an absolute path".to_string());
            }
            let text = std::fs::read_to_string(&manifest)
                .map_err(|e| format!("read {}: {e}", manifest.display()))?;
            let output = apply(kind, &text)?;
            Ok(json!({ "kind": kind, "applied": true, "output": output }))
        }
        other => Err(format!(
            "unknown provision action `{other}` (expected probe, list or apply)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_validates_actions_and_arguments() {
        assert!(
            dispatch(&json!({"action": "explode"}))
                .unwrap_err()
                .contains("unknown provision action")
        );
        assert!(
            dispatch(&json!({"action": "list"}))
                .unwrap_err()
                .contains("requires kind")
        );
        assert!(
            dispatch(&json!({"action": "apply", "kind": "kubernetes"}))
                .unwrap_err()
                .contains("requires manifest")
        );
        assert!(
            dispatch(&json!({"action": "apply", "kind": "kubernetes", "manifest": "m.yaml"}))
                .unwrap_err()
                .contains("absolute")
        );
        assert!(dispatch(&json!({"action": "list", "kind": "not-a-cloud"})).is_err());
    }

    #[test]
    fn dispatch_apply_rejects_blank_manifest_and_nonexistent_absolute_path() {
        assert!(
            dispatch(&json!({"action": "apply", "kind": "kubernetes", "manifest": "   "}))
                .unwrap_err()
                .contains("requires manifest")
        );
        let missing = std::env::temp_dir().join(format!(
            "susi-nonexistent-manifest-{}.yaml",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&missing);
        let err = dispatch(&json!({
            "action": "apply",
            "kind": "kubernetes",
            "manifest": missing.to_string_lossy(),
        }))
        .unwrap_err();
        assert!(
            err.contains("read "),
            "expected file read error, got: {err}"
        );
    }

    #[test]
    fn apply_and_list_report_reject_unknown_cloud_kind() {
        assert!(apply("not-a-cloud", "apiVersion: v1").is_err());
        assert!(list_report("not-a-cloud").is_err());
    }

    #[test]
    fn dispatch_apply_validates_kind_before_manifest() {
        assert!(
            dispatch(&json!({"action": "apply"}))
                .unwrap_err()
                .contains("apply requires kind")
        );
    }

    #[test]
    fn dispatch_rejects_blank_action_and_non_string_action_defaults_to_probe_contract() {
        assert!(
            dispatch(&json!({"action": "   "}))
                .unwrap_err()
                .contains("unknown provision action")
        );
        assert!(
            dispatch(&json!({"action": "delete"}))
                .unwrap_err()
                .contains("expected probe, list or apply")
        );
    }
}
