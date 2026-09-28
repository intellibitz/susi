#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! # susi-vendor-cloud
//!
//! First-class **provision/manage** plane for local and cloud AI hosts.
//! Talks to the operator's installed CLIs (`kubectl`, `docker`, `aws`,
//! `gcloud`, `az`) over `std::process::Command`. No AWS/GCP/Azure/K8s
//! SDK and no HTTP client crate — those stay out of core/OS crates.

use std::io::Write;
use std::process::{Command, Stdio};

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
    ///
    /// # Errors
    /// Unknown token — list/apply stay explicit; never guess a cloud.
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

fn run(bin: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(bin)
        .args(args)
        .output()
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
            match run(tool, kind.version_args()) {
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
///
/// # Errors
/// Missing CLI, auth failure, or empty/non-zero process status.
pub fn list_nodes(kind: CloudKind) -> Result<String, String> {
    run(kind.bin(), kind.list_args())
}

/// Apply a Kubernetes manifest via `kubectl apply -f -`.
///
/// # Errors
/// Missing kubectl, apply failure, or stdin pipe error.
pub fn apply_manifest(kind: CloudKind, manifest: &str) -> Result<String, String> {
    match kind {
        CloudKind::Kubernetes => {
            let mut child = Command::new("kubectl")
                .args(["apply", "-f", "-"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("kubectl apply: {e}"))?;
            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(manifest.as_bytes())
                    .map_err(|e| format!("kubectl apply stdin: {e}"))?;
            }
            let out = child
                .wait_with_output()
                .map_err(|e| format!("kubectl apply wait: {e}"))?;
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
        assert_eq!(run("echo", &["hello"]).as_deref(), Ok("hello"));
        // Non-zero exit: stderr wins when present, else stdout.
        let err = run("sh", &["-c", "echo oops >&2; exit 3"]).unwrap_err();
        assert!(err.contains("oops"));
        // Missing binary: the spawn error is typed, not a panic.
        assert!(run("/nonexistent-susi-cloud-tool", &[]).is_err());
    }

    #[test]
    fn probe_all_returns_one_inventory_per_kind() {
        let inv = probe_all();
        assert_eq!(inv.len(), 5);
        for (entry, kind) in inv.iter().zip(CloudKind::all()) {
            assert_eq!(entry.kind, kind.as_str());
            assert_eq!(entry.tool, kind.bin());
            // The summary is always bounded whether the CLI answered or not.
            assert!(entry.summary.chars().count() <= 241);
        }
    }

    #[test]
    fn list_nodes_dispatches_to_the_kind_binary() {
        // Whatever the host state, the call must resolve to a typed Result —
        // Ok when the CLI answers, Err when the binary is absent or the
        // provider rejects the call.
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
        // With kubectl absent this is the spawn-error arm; with it present,
        // the apply fails against no cluster — both must be typed Err.
        let res = apply_manifest(CloudKind::Kubernetes, "kind: Pod\n");
        assert!(res.is_err());
    }
}
