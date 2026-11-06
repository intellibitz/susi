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
                reason: "health check failed; rolled back to last known good revision".to_string(),
            };
        }
        observed.healthy = false;
        return ReconcileAction::Rollback {
            operation_id,
            restore_revision: String::new(),
            reason: "health check failed with no prior healthy revision".to_string(),
        };
    }

    let already_converged = observed.active_revision.as_deref() == Some(desired.revision.as_str())
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
}
