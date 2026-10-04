//! Cloud deployment reconciler desired/observed state tests (VC-201-055).

use crate::cloud_reconciler::{
    CloudReconciler, DeploymentDesiredState, DeploymentObservedState, DeploymentPhase,
    ReconcileAction,
};

#[test]
fn vc_201_055_cloud_reconciler_desired_observed_state() {
    let mut reconciler = CloudReconciler::new();

    let mut desired = DeploymentDesiredState {
        deployment_id: "dep-runpod-llama3".into(),
        target_backend: "runpod".into(),
        model_id: "meta-llama/Meta-Llama-3-70B".into(),
        min_replicas: 2,
        max_replicas: 5,
        gpu_type: "NVIDIA-A100-SXM4-80GB".into(),
        generation: 1,
    };

    let mut observed = DeploymentObservedState {
        deployment_id: "dep-runpod-llama3".into(),
        target_backend: "runpod".into(),
        model_id: "meta-llama/Meta-Llama-3-70B".into(),
        active_replicas: 0,
        healthy_replicas: 0,
        phase: DeploymentPhase::Pending,
        last_operation_id: None,
        observed_generation: 0,
    };

    // 1. Initial reconciliation provisions generation 1
    let outcome1 = reconciler.reconcile(&desired, &mut observed);
    assert_eq!(outcome1.action, ReconcileAction::Provision);
    assert!(!outcome1.deduplicated);
    assert_eq!(observed.observed_generation, 1);
    assert_eq!(observed.active_replicas, 2);
    assert_eq!(observed.phase, DeploymentPhase::Healthy);

    // 2. Health check probe succeeds, marking generation 1 as known-good
    reconciler.run_health_check(&desired, &mut observed, true);
    assert_eq!(observed.phase, DeploymentPhase::Healthy);
    assert_eq!(observed.healthy_replicas, 2);

    // 3. Repeated reconcile is a Noop (idempotency)
    let outcome2 = reconciler.reconcile(&desired, &mut observed);
    assert_eq!(outcome2.action, ReconcileAction::Noop);
    assert!(outcome2.deduplicated);

    // 4. Crash recovery deduplication: same operation is not re-executed
    let op_id = CloudReconciler::compute_operation_id(
        &desired.deployment_id,
        desired.generation,
        &desired.target_backend,
        ReconcileAction::Provision,
    );
    assert!(reconciler.is_operation_completed(&op_id));

    // 5. Update desired state to generation 2
    desired.generation = 2;
    desired.model_id = "meta-llama/Meta-Llama-3.1-70B".into();
    let outcome3 = reconciler.reconcile(&desired, &mut observed);
    assert_eq!(outcome3.action, ReconcileAction::Provision);
    assert_eq!(observed.observed_generation, 2);

    // 6. Generation 2 fails health check probe -> transitions to Degraded
    reconciler.run_health_check(&desired, &mut observed, false);
    assert_eq!(observed.phase, DeploymentPhase::Degraded);
    assert_eq!(observed.healthy_replicas, 0);

    // 7. Safe rollback returns to generation 1
    let rollback_outcome = reconciler
        .rollback(&mut desired, &mut observed)
        .expect("rollback to known-good generation 1 succeeds");
    assert_eq!(rollback_outcome.action, ReconcileAction::Rollback);
    assert_eq!(desired.generation, 1);
    assert_eq!(observed.observed_generation, 1);
    assert_eq!(observed.phase, DeploymentPhase::RolledBack);
    assert_eq!(observed.active_replicas, 2);
    assert_eq!(observed.healthy_replicas, 2);
}
