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

pub mod autoscale;
pub mod failover_policy;
pub mod orphan_gc;
pub mod preemptible;
pub mod reconcile {
    //! Managed cloud inference deployment reconciliation (`VC-201-055`).
    //!
    //! Converges desired and observed state across supported GPU cloud backends
    //! with deterministic operation IDs, idempotent crash recovery, and automatic
    //! rollback to the last-known-good revision on health check failure.

    /// Managed GPU cloud backends supported by the reconciliation loop.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum ManagedCloudBackend {
        Replicate,
        Modal,
        RunPod,
        Lambda,
        CoreWeave,
        Kubernetes,
    }

    impl ManagedCloudBackend {
        #[must_use]
        pub const fn as_str(self) -> &'static str {
            match self {
                Self::Replicate => "replicate",
                Self::Modal => "modal",
                Self::RunPod => "runpod",
                Self::Lambda => "lambda",
                Self::CoreWeave => "coreweave",
                Self::Kubernetes => "kubernetes",
            }
        }
    }

    /// Desired deployment specification for a managed cloud inference endpoint.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DesiredDeployment {
        pub deployment_id: String,
        pub backend: ManagedCloudBackend,
        pub model_id: String,
        pub revision: String,
        pub replicas: u32,
        pub image_digest: String,
    }

    /// Live observed state of a managed cloud inference deployment.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ObservedDeployment {
        pub deployment_id: String,
        pub active_revision: Option<String>,
        pub replicas: u32,
        pub healthy: bool,
        pub last_known_good_revision: Option<String>,
        pub applied_operation_ids: Vec<String>,
    }

    impl ObservedDeployment {
        #[must_use]
        pub fn new(deployment_id: impl Into<String>) -> Self {
            Self {
                deployment_id: deployment_id.into(),
                active_revision: None,
                replicas: 0,
                healthy: true,
                last_known_good_revision: None,
                applied_operation_ids: Vec::new(),
            }
        }
    }

    /// Reconciliation decision produced by comparing desired vs observed state.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ReconcileAction {
        /// Desired and observed states already match; no cloud mutation is issued.
        NoOp { operation_id: String },
        /// Apply the desired revision and replica count under a deterministic operation ID.
        Apply {
            operation_id: String,
            target_revision: String,
            replicas: u32,
        },
        /// Roll back to the last-known-good revision after a failed rollout or health check.
        Rollback {
            operation_id: String,
            restore_revision: String,
            reason: String,
        },
    }

    /// Compute a deterministic, collision-resistant operation ID from `desired`.
    ///
    /// Repeated `apply` calls or crash-recovery loops with the same desired
    /// specification derive the exact same operation ID so cloud resources are
    /// never duplicated.
    #[must_use]
    pub fn stable_operation_id(desired: &DesiredDeployment) -> String {
        const FNV_PRIME_A: u64 = 0x0000_0100_0000_01b3;
        const FNV_PRIME_B: u64 = 0x0000_0100_0000_01d9;
        let mut hash_a: u64 = 0xcbf2_9ce4_8422_2325;
        let mut hash_b: u64 = 0x8422_2325_cbf2_9ce4;

        let mut feed = |field: &[u8]| {
            let len_bytes = (field.len() as u64).to_le_bytes();
            for &b in len_bytes.iter().chain(field.iter()) {
                hash_a ^= u64::from(b);
                hash_a = hash_a.wrapping_mul(FNV_PRIME_A);
                hash_b = hash_b.wrapping_add(u64::from(b)).wrapping_mul(FNV_PRIME_B);
            }
            hash_a ^= 0xff;
            hash_b ^= 0xa5;
        };

        feed(desired.deployment_id.as_bytes());
        feed(desired.backend.as_str().as_bytes());
        feed(desired.model_id.as_bytes());
        feed(desired.revision.as_bytes());
        feed(&desired.replicas.to_le_bytes());
        feed(desired.image_digest.as_bytes());

        format!("op-{hash_a:016x}{hash_b:016x}")
    }

    /// Execute one deterministic reconciliation step between `desired` and `observed`.
    ///
    /// - If `health_probe_passed` is false or `observed` is unhealthy, rolls back
    ///   to `last_known_good_revision` and emits [`ReconcileAction::Rollback`].
    /// - If `observed` already matches `desired` (or `operation_id` was already
    ///   recorded during a prior interrupted apply that passed health checks),
    ///   converges without duplicating resources and emits [`ReconcileAction::NoOp`].
    /// - Otherwise applies `desired` and emits [`ReconcileAction::Apply`].
    pub fn reconcile_step(
        desired: &DesiredDeployment,
        observed: &mut ObservedDeployment,
        health_probe_passed: bool,
    ) -> ReconcileAction {
        let operation_id = stable_operation_id(desired);

        if !health_probe_passed || !observed.healthy {
            if let Some(good) = observed.last_known_good_revision.clone() {
                observed.active_revision = Some(good.clone());
                observed.healthy = true;
                return ReconcileAction::Rollback {
                    operation_id,
                    restore_revision: good,
                    reason: "health check failed; rolled back to last known good revision"
                        .to_string(),
                };
            }
            observed.healthy = false;
            return ReconcileAction::Rollback {
                operation_id,
                restore_revision: String::new(),
                reason: "health check failed with no prior healthy revision".to_string(),
            };
        }

        let already_converged = observed.active_revision.as_deref()
            == Some(desired.revision.as_str())
            && observed.replicas == desired.replicas
            && observed.healthy;
        if already_converged {
            if observed.last_known_good_revision.is_none() {
                observed.last_known_good_revision = Some(desired.revision.clone());
            }
            if !observed.applied_operation_ids.contains(&operation_id) {
                observed.applied_operation_ids.push(operation_id.clone());
            }
            return ReconcileAction::NoOp { operation_id };
        }

        if observed.applied_operation_ids.contains(&operation_id) {
            observed.active_revision = Some(desired.revision.clone());
            observed.replicas = desired.replicas;
            observed.healthy = true;
            if observed.last_known_good_revision.is_none() {
                observed.last_known_good_revision = Some(desired.revision.clone());
            }
            return ReconcileAction::NoOp { operation_id };
        }

        if let Some(prev) = observed.active_revision.clone() {
            observed.last_known_good_revision = Some(prev);
        } else if observed.last_known_good_revision.is_none() {
            observed.last_known_good_revision = Some(desired.revision.clone());
        }
        observed.active_revision = Some(desired.revision.clone());
        observed.replicas = desired.replicas;
        observed.healthy = true;
        observed.applied_operation_ids.push(operation_id.clone());

        ReconcileAction::Apply {
            operation_id,
            target_revision: desired.revision.clone(),
            replicas: desired.replicas,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn sample_desired(revision: &str, replicas: u32) -> DesiredDeployment {
            DesiredDeployment {
                deployment_id: "dep-replicate-llama".to_string(),
                backend: ManagedCloudBackend::Replicate,
                model_id: "meta/llama-3.3-70b".to_string(),
                revision: revision.to_string(),
                replicas,
                image_digest: format!("sha256:{revision}"),
            }
        }

        #[test]
        fn stable_operation_id_is_deterministic_and_sensitive_to_spec_changes() {
            let d1 = sample_desired("rev-1", 2);
            let d2 = sample_desired("rev-1", 2);
            let d3 = sample_desired("rev-2", 2);
            assert_eq!(stable_operation_id(&d1), stable_operation_id(&d2));
            assert_ne!(stable_operation_id(&d1), stable_operation_id(&d3));
        }

        fn desired(
            backend: ManagedCloudBackend,
            revision: &str,
            replicas: u32,
        ) -> DesiredDeployment {
            DesiredDeployment {
                deployment_id: format!("dep-{}", backend.as_str()),
                backend,
                model_id: "deepseek-ai/DeepSeek-V3".to_string(),
                revision: revision.to_string(),
                replicas,
                image_digest: format!("sha256:img-{revision}"),
            }
        }

        #[test]
        fn vc_201_055_reconciles_desired_and_observed_state_with_stable_operation_ids() {
            for backend in [
                ManagedCloudBackend::Replicate,
                ManagedCloudBackend::Modal,
                ManagedCloudBackend::RunPod,
                ManagedCloudBackend::Lambda,
                ManagedCloudBackend::CoreWeave,
                ManagedCloudBackend::Kubernetes,
            ] {
                let spec = desired(backend, "rev-1", 3);
                let expected_op = stable_operation_id(&spec);
                let mut observed = ObservedDeployment::new(&spec.deployment_id);

                let action = reconcile_step(&spec, &mut observed, true);
                assert_eq!(
                    action,
                    ReconcileAction::Apply {
                        operation_id: expected_op.clone(),
                        target_revision: "rev-1".to_string(),
                        replicas: 3,
                    }
                );
                assert_eq!(observed.active_revision.as_deref(), Some("rev-1"));
                assert_eq!(observed.replicas, 3);
                assert!(observed.healthy);
                assert_eq!(observed.last_known_good_revision.as_deref(), Some("rev-1"));
                assert_eq!(observed.applied_operation_ids, vec![expected_op]);
            }
        }

        #[test]
        fn vc_201_055_repeated_apply_and_crash_recovery_do_not_create_duplicate_resources() {
            let spec = desired(ManagedCloudBackend::RunPod, "rev-10", 4);
            let op_id = stable_operation_id(&spec);
            let mut observed = ObservedDeployment::new(&spec.deployment_id);

            // First apply converges state.
            let first = reconcile_step(&spec, &mut observed, true);
            assert!(matches!(first, ReconcileAction::Apply { .. }));
            assert_eq!(observed.applied_operation_ids.len(), 1);

            // Repeated apply on already-converged state is a pure NoOp.
            let second = reconcile_step(&spec, &mut observed, true);
            assert_eq!(
                second,
                ReconcileAction::NoOp {
                    operation_id: op_id.clone()
                }
            );
            assert_eq!(
                observed.applied_operation_ids.len(),
                1,
                "repeated apply must not record duplicate operation IDs"
            );

            // Simulate crash recovery where the operation ID was recorded remotely before the local
            // active_revision / replica fields finished updating.
            let mut recovering = ObservedDeployment {
                deployment_id: spec.deployment_id.clone(),
                active_revision: None,
                replicas: 0,
                healthy: true,
                last_known_good_revision: None,
                applied_operation_ids: vec![op_id.clone()],
            };
            let recovered = reconcile_step(&spec, &mut recovering, true);
            assert_eq!(
        recovered,
        ReconcileAction::NoOp {
            operation_id: op_id
        },
        "crash recovery for an already-applied operation ID must converge idempotently without re-applying"
    );
            assert_eq!(recovering.active_revision.as_deref(), Some("rev-10"));
            assert_eq!(recovering.replicas, 4);
            assert_eq!(recovering.applied_operation_ids.len(), 1);
        }

        #[test]
        fn vc_201_055_health_check_failure_rolls_back_to_last_known_good_revision() {
            let rev1 = desired(ManagedCloudBackend::Modal, "rev-1", 2);
            let rev2 = desired(ManagedCloudBackend::Modal, "rev-2", 2);
            let mut observed = ObservedDeployment::new(&rev1.deployment_id);

            // Establish rev-1 as healthy last-known-good.
            let initial = reconcile_step(&rev1, &mut observed, true);
            assert!(matches!(initial, ReconcileAction::Apply { .. }));
            assert_eq!(observed.active_revision.as_deref(), Some("rev-1"));
            assert_eq!(observed.last_known_good_revision.as_deref(), Some("rev-1"));

            // Roll out rev-2, which fails its health probe.
            let failed_rollout = reconcile_step(&rev2, &mut observed, false);
            assert_eq!(
                failed_rollout,
                ReconcileAction::Rollback {
                    operation_id: stable_operation_id(&rev2),
                    restore_revision: "rev-1".to_string(),
                    reason: "health check failed; rolled back to last known good revision"
                        .to_string(),
                }
            );
            assert_eq!(
                observed.active_revision.as_deref(),
                Some("rev-1"),
                "failed rollout must restore active_revision to rev-1"
            );
            assert!(observed.healthy);
        }
    }
}

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
#[path = "tests/vc_201_056.rs"]
mod vc_201_056_tests;
#[cfg(test)]
#[path = "tests/vc_201_057.rs"]
mod vc_201_057_tests;
#[cfg(test)]
#[path = "tests/vc_201_058.rs"]
mod vc_201_058_tests;
#[cfg(test)]
#[path = "tests/vc_201_059.rs"]
mod vc_201_059_tests;

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
            run("cmd.exe", &["/d", "/c", "echo hello"]),
            run("cmd.exe", &["/d", "/c", "echo oops 1>&2 & exit /b 3"]),
        );
        #[cfg(not(windows))]
        let (success, failure) = (
            run("echo", &["hello"]),
            run("sh", &["-c", "echo oops >&2; exit 3"]),
        );
        assert_eq!(success.as_deref(), Ok("hello"));
        // Fixed fixture commands only; arbitrary production commands still use argv.
        let err = failure.unwrap_err();
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
