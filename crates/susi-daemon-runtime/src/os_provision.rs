//! OS provision callers forward to `daemon.os.provision` on `susi-plug-host`.
//!
//! `src/cli/os_cli.rs` still calls these library functions; the
//! implementations live in Susi Commercial Apps. Public Rust facades keep
//! the original types and signatures.

use serde_json::{Value, json};
use susi_core::plane_bus::{daemon, topics};

/// One cloud/local substrate this plane can inventory or mutate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudKind {
    Kubernetes,
    Docker,
    Aws,
    Gcp,
    Azure,
}

impl CloudKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kubernetes => "kubernetes",
            Self::Docker => "docker",
            Self::Aws => "aws",
            Self::Gcp => "gcp",
            Self::Azure => "azure",
        }
    }

    fn bin(self) -> &'static str {
        match self {
            Self::Kubernetes => "kubectl",
            Self::Docker => "docker",
            Self::Aws => "aws",
            Self::Gcp => "gcloud",
            Self::Azure => "az",
        }
    }

    /// Every kind this plane knows.
    #[must_use]
    pub fn all() -> [CloudKind; 5] {
        [
            Self::Kubernetes,
            Self::Docker,
            Self::Aws,
            Self::Gcp,
            Self::Azure,
        ]
    }

    /// Parse an operator-facing kind name (`kubernetes`/`k8s`/`kubectl`,
    /// `docker`, `aws`, `gcp`/`gcloud`, `azure`/`az`).
    pub fn parse(name: &str) -> Result<Self, String> {
        match name.trim().to_ascii_lowercase().as_str() {
            "k8s" | "kubernetes" | "kubectl" => Ok(Self::Kubernetes),
            "docker" => Ok(Self::Docker),
            "aws" => Ok(Self::Aws),
            "gcp" | "gcloud" | "google" => Ok(Self::Gcp),
            "azure" | "az" => Ok(Self::Azure),
            other => Err(format!(
                "unknown cloud kind '{other}'; expected kubernetes|docker|aws|gcp|azure"
            )),
        }
    }
}

/// CLI probe result ? availability of the operator tool, not a fake cluster.
#[derive(Debug, Clone)]
pub struct CloudInventory {
    pub kind: &'static str,
    pub tool: &'static str,
    pub available: bool,
    pub summary: String,
}

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

fn inventory_from_value(value: &Value) -> Option<CloudInventory> {
    let kind = CloudKind::parse(value.get("kind").and_then(Value::as_str).unwrap_or("")).ok()?;
    Some(CloudInventory {
        kind: kind.as_str(),
        tool: kind.bin(),
        available: value
            .get("available")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        summary: value
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    })
}

/// Probe whether each operator CLI is on PATH (version only ? no cloud API).
#[must_use]
pub fn probe_all() -> Vec<CloudInventory> {
    match call_plug(daemon::os_provision(json!({ "action": "probe" }))) {
        Ok(value) => value
            .get("inventory")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(inventory_from_value)
            .collect(),
        Err(summary) => CloudKind::all()
            .into_iter()
            .map(|kind| CloudInventory {
                kind: kind.as_str(),
                tool: kind.bin(),
                available: false,
                summary: summary.clone(),
            })
            .collect(),
    }
}

/// List nodes/instances. Hits the live API/cluster; caller must opt in.
pub fn list_nodes(kind: CloudKind) -> Result<String, String> {
    let value = list_report(kind.as_str())?;
    if let Some(output) = value.get("output").and_then(Value::as_str) {
        Ok(output.to_owned())
    } else if let Some(error) = value.get("error").and_then(Value::as_str) {
        Err(error.to_owned())
    } else {
        Ok(value.to_string())
    }
}

/// Apply a Kubernetes manifest via the plug host. Other kinds are refused
/// with the same kubectl-only contract as before.
pub fn apply_manifest(kind: CloudKind, manifest: &str) -> Result<String, String> {
    match kind {
        CloudKind::Kubernetes => apply(kind.as_str(), manifest),
        CloudKind::Docker | CloudKind::Aws | CloudKind::Gcp | CloudKind::Azure => Err(format!(
            "apply_manifest is kubectl-only; {} uses list_nodes / the provider CLI",
            kind.as_str()
        )),
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
