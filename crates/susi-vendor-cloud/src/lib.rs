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
