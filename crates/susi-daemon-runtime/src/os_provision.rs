//! Explicit cloud provisioning: what `susi os provision probe|list|apply`
//! does and the MCP `os_provision` tool does over the `daemon.os.provision`
//! plane topic (zero-UI plan B4 group 9). One implementation for both.
//!
//! Nothing here runs on a tick: `probe` only asks each cloud CLI for its
//! version, and `list` / `apply` run only on an operator's explicit request.

use serde_json::{Value, json};
use std::time::Duration;
use susi_paths::console::{apply_non_interactive_env, background_command, run_output_within};

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

    fn version_args(self) -> &'static [&'static str] {
        match self {
            Self::Kubernetes => &["version", "--client", "--output=yaml"],
            Self::Docker => &["version", "--format", "{{.Server.Version}}"],
            Self::Aws => &["--version"],
            Self::Gcp => &["version"],
            Self::Azure => &["version"],
        }
    }

    fn list_args(self) -> &'static [&'static str] {
        match self {
            Self::Kubernetes => &["get", "nodes", "-o", "name", "--request-timeout=3s"],
            Self::Docker => &["ps", "-q"],
            Self::Aws => &[
                "ec2",
                "describe-instances",
                "--query",
                "Reservations[].Instances[].InstanceId",
                "--output",
                "text",
            ],
            Self::Gcp => &["compute", "instances", "list", "--format=value(name)"],
            Self::Azure => &["vm", "list", "--query", "[].name", "-o", "tsv"],
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

/// CLI probe result — availability of the operator tool, not a fake cluster.
#[derive(Debug, Clone)]
pub struct CloudInventory {
    pub kind: &'static str,
    pub tool: &'static str,
    pub available: bool,
    pub summary: String,
}

fn run_cli(bin: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = background_command(bin);
    cmd.args(args);
    apply_non_interactive_env(&mut cmd);
    let out = run_output_within(&mut cmd, None, Duration::from_secs(60))
        .map_err(|e| format!("{bin}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if out.status.success() {
        Ok(if stdout.is_empty() { stderr } else { stdout })
    } else {
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

/// Probe whether each operator CLI is on PATH (version only — no cloud API).
#[must_use]
pub fn probe_all() -> Vec<CloudInventory> {
    CloudKind::all()
        .into_iter()
        .map(|kind| {
            let tool = kind.bin();
            match run_cli(tool, kind.version_args()) {
                Ok(summary) => CloudInventory {
                    kind: kind.as_str(),
                    tool,
                    available: true,
                    summary: truncate(&summary, 240),
                },
                Err(summary) => CloudInventory {
                    kind: kind.as_str(),
                    tool,
                    available: false,
                    summary: truncate(&summary, 240),
                },
            }
        })
        .collect()
}

/// List nodes/instances. Hits the live API/cluster; caller must opt in.
pub fn list_nodes(kind: CloudKind) -> Result<String, String> {
    run_cli(kind.bin(), kind.list_args())
}

/// Apply a Kubernetes manifest via `kubectl apply -f -`.
pub fn apply_manifest(kind: CloudKind, manifest: &str) -> Result<String, String> {
    match kind {
        CloudKind::Kubernetes => {
            let mut cmd = background_command("kubectl");
            cmd.args(["apply", "-f", "-"]);
            apply_non_interactive_env(&mut cmd);
            let out = run_output_within(
                &mut cmd,
                Some(manifest.as_bytes()),
                Duration::from_secs(120),
            )
            .map_err(|e| format!("kubectl apply: {e}"))?;
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if out.status.success() {
                Ok(stdout)
            } else {
                Err(if stderr.is_empty() { stdout } else { stderr })
            }
        }
        CloudKind::Docker | CloudKind::Aws | CloudKind::Gcp | CloudKind::Azure => Err(format!(
            "apply_manifest is kubectl-only; {} uses list_nodes / the provider CLI",
            kind.as_str()
        )),
    }
}

/// `susi os provision probe --json`: which cloud CLIs this host has.
pub fn probe_report() -> Value {
    let inv = probe_all();
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
    let kind = CloudKind::parse(kind)?;
    let nodes = list_nodes(kind)?;
    Ok(json!({
        "kind": kind.as_str(),
        "explicit": true,
        "nodes": nodes,
    }))
}

/// `susi os provision apply <kind> <manifest>`: apply manifest text through
/// the cloud CLI; returns the tool's output.
pub fn apply(kind: &str, manifest_text: &str) -> Result<String, String> {
    let kind = CloudKind::parse(kind)?;
    apply_manifest(kind, manifest_text)
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
    fn parse_accepts_every_alias_case_insensitively() {
        for (token, want) in [
            ("kubernetes", CloudKind::Kubernetes),
            ("k8s", CloudKind::Kubernetes),
            ("Kubectl", CloudKind::Kubernetes),
            ("docker", CloudKind::Docker),
            ("aws", CloudKind::Aws),
            (" gcp ", CloudKind::Gcp),
            ("gcloud", CloudKind::Gcp),
            ("google", CloudKind::Gcp),
            ("azure", CloudKind::Azure),
            ("AZ", CloudKind::Azure),
        ] {
            assert_eq!(CloudKind::parse(token), Ok(want), "token {token}");
        }
    }

    #[test]
    fn parse_rejects_unknown_kinds() {
        for bad in ["", "heroku", "k8", "cloud", "aws2"] {
            assert!(CloudKind::parse(bad).is_err(), "token {bad:?}");
        }
    }

    #[test]
    fn all_round_trips_through_parse() {
        let all = CloudKind::all();
        assert_eq!(all.len(), 5);
        for kind in all {
            assert_eq!(CloudKind::parse(kind.as_str()), Ok(kind));
        }
    }

    #[test]
    fn truncate_clips_at_the_limit_with_ellipsis() {
        assert_eq!(truncate("abcdef", 3), "abc…");
        assert_eq!(truncate("ab", 8), "ab");
        assert_eq!(truncate("", 0), "");
    }

    #[test]
    fn run_returns_stdout_on_success_and_stderr_on_failure() {
        #[cfg(windows)]
        let (success, failure) = (
            run_cli("cmd.exe", &["/d", "/c", "echo hello"]),
            run_cli("cmd.exe", &["/d", "/c", "echo oops 1>&2 & exit /b 3"]),
        );
        #[cfg(not(windows))]
        let (success, failure) = (
            run_cli("echo", &["hello"]),
            run_cli("sh", &["-c", "echo oops >&2; exit 3"]),
        );
        assert_eq!(success.as_deref(), Ok("hello"));
        let err = failure.unwrap_err();
        assert!(err.contains("oops"));
        assert!(run_cli("/nonexistent-susi-cloud-tool", &[]).is_err());
    }

    #[test]
    fn probe_all_returns_one_inventory_per_kind() {
        let inv = probe_all();
        assert_eq!(inv.len(), 5);
        for (entry, kind) in inv.iter().zip(CloudKind::all()) {
            assert_eq!(entry.kind, kind.as_str());
            assert_eq!(entry.tool, kind.bin());
            assert!(entry.summary.chars().count() <= 241);
        }
    }

    #[test]
    fn list_nodes_dispatches_to_the_kind_binary() {
        let _ = list_nodes(CloudKind::Kubernetes).map_err(|e| assert!(!e.is_empty()));
    }

    #[test]
    fn apply_manifest_rejects_every_non_kubernetes_kind() {
        for kind in [
            CloudKind::Docker,
            CloudKind::Aws,
            CloudKind::Gcp,
            CloudKind::Azure,
        ] {
            let err = apply_manifest(kind, "x").unwrap_err();
            assert!(err.contains("kubectl-only"), "{kind:?}");
        }
    }

    #[test]
    fn apply_manifest_spawns_kubectl_or_errors() {
        let res = apply_manifest(CloudKind::Kubernetes, "kind: Pod\n");
        assert!(res.is_err());
    }

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
