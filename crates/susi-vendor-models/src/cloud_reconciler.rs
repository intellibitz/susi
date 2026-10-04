//! Managed cloud inference deployment reconciler (VC-201-055).
//!
//! Extends static GPU cloud profiles with active desired/observed state
//! reconciliation, deterministic operation IDs for crash recovery deduplication,
//! health checks, and automatic rollback on degraded provisioning.

use std::collections::{BTreeMap, BTreeSet};

/// Lifecycle phases of a managed cloud deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentPhase {
    Pending,
    Provisioning,
    Healthy,
    Degraded,
    RollingBack,
    RolledBack,
    Failed,
}

/// Desired state for a managed cloud model deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentDesiredState {
    pub deployment_id: String,
    pub target_backend: String,
    pub model_id: String,
    pub min_replicas: u32,
    pub max_replicas: u32,
    pub gpu_type: String,
    pub generation: u64,
}

/// Observed state reported by cloud provider probes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentObservedState {
    pub deployment_id: String,
    pub target_backend: String,
    pub model_id: String,
    pub active_replicas: u32,
    pub healthy_replicas: u32,
    pub phase: DeploymentPhase,
    pub last_operation_id: Option<String>,
    pub observed_generation: u64,
}

/// Action determined by the reconciler to advance observed toward desired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileAction {
    Noop,
    Provision,
    ScaleUp,
    ScaleDown,
    HealthCheckFailed,
    Rollback,
}

/// Outcome of a single reconciliation cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileOutcome {
    pub action: ReconcileAction,
    pub operation_id: String,
    pub deduplicated: bool,
    pub message: String,
}

/// Active provisioning reconciler tracking operations and generation history.
#[derive(Debug, Default, Clone)]
pub struct CloudReconciler {
    completed_operations: BTreeSet<String>,
    generation_history: BTreeMap<String, Vec<DeploymentDesiredState>>,
}

impl CloudReconciler {
    #[must_use]
    pub fn new() -> Self {
        Self {
            completed_operations: BTreeSet::new(),
            generation_history: BTreeMap::new(),
        }
    }

    /// Compute a stable deterministic operation ID for a state transition.
    ///
    /// The ID is generated from deployment attributes and action using FNV-1a,
    /// guaranteeing identical IDs across process restarts and re-entrant calls.
    #[must_use]
    pub fn compute_operation_id(
        deployment_id: &str,
        generation: u64,
        backend: &str,
        action: ReconcileAction,
    ) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in deployment_id.as_bytes() {
            h = fnv1a(h, *b);
        }
        for b in generation.to_le_bytes() {
            h = fnv1a(h, b);
        }
        for b in backend.as_bytes() {
            h = fnv1a(h, *b);
        }
        let action_tag = match action {
            ReconcileAction::Noop => 0u8,
            ReconcileAction::Provision => 1u8,
            ReconcileAction::ScaleUp => 2u8,
            ReconcileAction::ScaleDown => 3u8,
            ReconcileAction::HealthCheckFailed => 4u8,
            ReconcileAction::Rollback => 5u8,
        };
        h = fnv1a(h, action_tag);
        format!("op-{deployment_id}-gen{generation}-{h:016x}")
    }

    /// Check if an operation has already executed (crash recovery deduplication).
    #[must_use]
    pub fn is_operation_completed(&self, op_id: &str) -> bool {
        self.completed_operations.contains(op_id)
    }

    /// Record an operation as completed.
    pub fn record_completed_operation(&mut self, op_id: String) {
        self.completed_operations.insert(op_id);
    }

    /// Save a known-good desired state in generation history for potential rollback.
    pub fn record_known_good(&mut self, desired: &DeploymentDesiredState) {
        let history = self
            .generation_history
            .entry(desired.deployment_id.clone())
            .or_default();
        if !history.iter().any(|d| d.generation == desired.generation) {
            history.push(desired.clone());
        }
    }

    /// Apply reconciliation between desired and observed state.
    ///
    /// Returns the outcome detailing the action taken, operation ID, and whether
    /// the operation was deduplicated.
    pub fn reconcile(
        &mut self,
        desired: &DeploymentDesiredState,
        observed: &mut DeploymentObservedState,
    ) -> ReconcileOutcome {
        if observed.phase == DeploymentPhase::Degraded || observed.phase == DeploymentPhase::Failed
        {
            let op_id = Self::compute_operation_id(
                &desired.deployment_id,
                desired.generation,
                &desired.target_backend,
                ReconcileAction::Rollback,
            );
            let deduplicated = self.is_operation_completed(&op_id);
            if !deduplicated {
                self.record_completed_operation(op_id.clone());
            }
            observed.phase = DeploymentPhase::RollingBack;
            observed.last_operation_id = Some(op_id.clone());
            return ReconcileOutcome {
                action: ReconcileAction::Rollback,
                operation_id: op_id,
                deduplicated,
                message: "Deployment degraded or failed; triggered rollback".into(),
            };
        }

        if observed.observed_generation != desired.generation {
            let op_id = Self::compute_operation_id(
                &desired.deployment_id,
                desired.generation,
                &desired.target_backend,
                ReconcileAction::Provision,
            );
            let deduplicated = self.is_operation_completed(&op_id);
            if !deduplicated {
                self.record_completed_operation(op_id.clone());
                observed.observed_generation = desired.generation;
                observed.model_id = desired.model_id.clone();
                observed.target_backend = desired.target_backend.clone();
                observed.active_replicas = desired.min_replicas;
                observed.healthy_replicas = desired.min_replicas;
                observed.phase = DeploymentPhase::Healthy;
            }
            observed.last_operation_id = Some(op_id.clone());
            return ReconcileOutcome {
                action: ReconcileAction::Provision,
                operation_id: op_id,
                deduplicated,
                message: format!(
                    "Provisioned generation {} on backend {}",
                    desired.generation, desired.target_backend
                ),
            };
        }

        if observed.active_replicas < desired.min_replicas {
            let op_id = Self::compute_operation_id(
                &desired.deployment_id,
                desired.generation,
                &desired.target_backend,
                ReconcileAction::ScaleUp,
            );
            let deduplicated = self.is_operation_completed(&op_id);
            if !deduplicated {
                self.record_completed_operation(op_id.clone());
                observed.active_replicas = desired.min_replicas;
                observed.healthy_replicas = desired.min_replicas;
                observed.phase = DeploymentPhase::Healthy;
            }
            observed.last_operation_id = Some(op_id.clone());
            return ReconcileOutcome {
                action: ReconcileAction::ScaleUp,
                operation_id: op_id,
                deduplicated,
                message: format!("Scaled up to {} replicas", desired.min_replicas),
            };
        }

        if observed.active_replicas > desired.max_replicas {
            let op_id = Self::compute_operation_id(
                &desired.deployment_id,
                desired.generation,
                &desired.target_backend,
                ReconcileAction::ScaleDown,
            );
            let deduplicated = self.is_operation_completed(&op_id);
            if !deduplicated {
                self.record_completed_operation(op_id.clone());
                observed.active_replicas = desired.max_replicas;
                if observed.healthy_replicas > desired.max_replicas {
                    observed.healthy_replicas = desired.max_replicas;
                }
            }
            observed.last_operation_id = Some(op_id.clone());
            return ReconcileOutcome {
                action: ReconcileAction::ScaleDown,
                operation_id: op_id,
                deduplicated,
                message: format!("Scaled down to {} replicas", desired.max_replicas),
            };
        }

        let op_id = Self::compute_operation_id(
            &desired.deployment_id,
            desired.generation,
            &desired.target_backend,
            ReconcileAction::Noop,
        );
        observed.last_operation_id = Some(op_id.clone());
        ReconcileOutcome {
            action: ReconcileAction::Noop,
            operation_id: op_id,
            deduplicated: true,
            message: "Observed state already matches desired state".into(),
        }
    }

    /// Perform a health check probe on the observed deployment.
    pub fn run_health_check(
        &mut self,
        desired: &DeploymentDesiredState,
        observed: &mut DeploymentObservedState,
        probe_ok: bool,
    ) {
        if probe_ok {
            observed.healthy_replicas = observed.active_replicas;
            if observed.healthy_replicas >= desired.min_replicas {
                observed.phase = DeploymentPhase::Healthy;
                self.record_known_good(desired);
            }
        } else {
            observed.healthy_replicas = 0;
            observed.phase = DeploymentPhase::Degraded;
        }
    }

    /// Execute rollback to previous known-good generation if available.
    pub fn rollback(
        &mut self,
        desired: &mut DeploymentDesiredState,
        observed: &mut DeploymentObservedState,
    ) -> Result<ReconcileOutcome, String> {
        let history = self
            .generation_history
            .get_mut(&desired.deployment_id)
            .ok_or_else(|| "No generation history found for deployment".to_string())?;

        let previous = history
            .iter()
            .rev()
            .find(|d| d.generation < desired.generation)
            .cloned()
            .ok_or_else(|| "No earlier known-good generation to rollback to".to_string())?;

        let op_id = Self::compute_operation_id(
            &desired.deployment_id,
            previous.generation,
            &previous.target_backend,
            ReconcileAction::Rollback,
        );

        *desired = previous.clone();
        observed.observed_generation = previous.generation;
        observed.model_id = previous.model_id;
        observed.target_backend = previous.target_backend;
        observed.active_replicas = previous.min_replicas;
        observed.healthy_replicas = previous.min_replicas;
        observed.phase = DeploymentPhase::RolledBack;
        observed.last_operation_id = Some(op_id.clone());

        self.record_completed_operation(op_id.clone());

        Ok(ReconcileOutcome {
            action: ReconcileAction::Rollback,
            operation_id: op_id,
            deduplicated: false,
            message: format!("Rolled back to generation {}", previous.generation),
        })
    }
}

const fn fnv1a(h: u64, b: u8) -> u64 {
    (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
}
