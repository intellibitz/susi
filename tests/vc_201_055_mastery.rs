#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! VC-201-055: Reconcile managed cloud inference deployments.
//!
//! Mastery criteria:
//! 1. Desired and Observed state reconciliation: convergent lifecycle phases
//!    (Pending -> Provisioning -> Healthy).
//! 2. Deterministic stable operation IDs: compute_operation_id generates stable
//!    operation hashes preventing duplicate actions under retries.
//! 3. Health checks and automated rollback: failing health check probes on a
//!    new generation trigger automated rollback to the previous known-good generation.
//! 4. Crash recovery deduplication: re-executing reconcile after simulated crash
//!    detects completed operations and deduplicates actions without spawning duplicate cloud resources.

use susi_vendor_models::cloud_reconciler::{
    CloudReconciler, DeploymentDesiredState, DeploymentObservedState, DeploymentPhase,
    ReconcileAction,
};

#[test]
fn vc_201_055_mastery() {
    let mut reconciler = CloudReconciler::new();

    // 1. Desired state for initial generation 1
    let mut desired = DeploymentDesiredState {
        deployment_id: "dep-mastery-cloud".into(),
        target_backend: "runpod".into(),
        model_id: "deepseek-ai/DeepSeek-Coder-V2-Lite".into(),
        min_replicas: 2,
        max_replicas: 4,
        gpu_type: "A100".into(),
        generation: 1,
    };

    let mut observed = DeploymentObservedState {
        deployment_id: "dep-mastery-cloud".into(),
        target_backend: "runpod".into(),
        model_id: "deepseek-ai/DeepSeek-Coder-V2-Lite".into(),
        active_replicas: 0,
        healthy_replicas: 0,
        phase: DeploymentPhase::Pending,
        last_operation_id: None,
        observed_generation: 0,
    };

    // 2. Initial reconciliation provisions generation 1
    let outcome1 = reconciler.reconcile(&desired, &mut observed);
    assert_eq!(outcome1.action, ReconcileAction::Provision);
    assert!(!outcome1.deduplicated);
    assert_eq!(observed.observed_generation, 1);
    assert_eq!(observed.phase, DeploymentPhase::Healthy);
    assert_eq!(observed.active_replicas, 2);

    // 3. Health check probe confirms generation 1 as known-good
    reconciler.run_health_check(&desired, &mut observed, true);
    assert_eq!(observed.phase, DeploymentPhase::Healthy);
    assert_eq!(observed.healthy_replicas, 2);

    // 4. Idempotency: repeated apply yields Noop with deduplication
    let outcome2 = reconciler.reconcile(&desired, &mut observed);
    assert_eq!(outcome2.action, ReconcileAction::Noop);
    assert!(outcome2.deduplicated);

    // 5. Crash recovery: simulated crash before observed state persisted
    let mut crash_recovered_observed = DeploymentObservedState {
        deployment_id: "dep-mastery-cloud".into(),
        target_backend: "runpod".into(),
        model_id: "deepseek-ai/DeepSeek-Coder-V2-Lite".into(),
        active_replicas: 0,
        healthy_replicas: 0,
        phase: DeploymentPhase::Pending,
        last_operation_id: None,
        observed_generation: 0,
    };
    let outcome_recovered = reconciler.reconcile(&desired, &mut crash_recovered_observed);
    assert!(
        outcome_recovered.deduplicated,
        "Crash recovery must deduplicate already-executed operation"
    );
    assert_eq!(outcome1.operation_id, outcome_recovered.operation_id);

    // 6. Update to generation 2
    desired.generation = 2;
    desired.model_id = "deepseek-ai/DeepSeek-Coder-V2-Lite-v2".into();
    let outcome3 = reconciler.reconcile(&desired, &mut observed);
    assert_eq!(outcome3.action, ReconcileAction::Provision);
    assert_eq!(observed.observed_generation, 2);

    // 7. Degradation and automated rollback: failing health check on gen 2 rolls back to gen 1
    reconciler.run_health_check(&desired, &mut observed, false);
    assert_eq!(observed.phase, DeploymentPhase::Degraded);
    assert_eq!(observed.healthy_replicas, 0);

    let rollback_outcome = reconciler
        .rollback(&mut desired, &mut observed)
        .expect("rollback to known-good generation 1 succeeds");
    assert_eq!(rollback_outcome.action, ReconcileAction::Rollback);
    assert_eq!(desired.generation, 1);
    assert_eq!(
        observed.observed_generation, 1,
        "Reconciler rolled back to known-good generation 1"
    );
    assert_eq!(observed.phase, DeploymentPhase::RolledBack);
    assert_eq!(
        observed.active_replicas, 2,
        "Replica count restored to generation 1 target"
    );
    assert_eq!(observed.healthy_replicas, 2);
}
