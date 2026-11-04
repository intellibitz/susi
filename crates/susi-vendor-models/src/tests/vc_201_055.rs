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

#[test]
fn vc_201_055_scale_out_and_scale_in_adjust_replicas_within_desired_bounds() {
    let mut reconciler = CloudReconciler::new();
    let desired = DeploymentDesiredState {
        deployment_id: "dep-scale".into(),
        target_backend: "runpod".into(),
        model_id: "qwen-72b".into(),
        min_replicas: 3,
        max_replicas: 5,
        gpu_type: "A100".into(),
        generation: 1,
    };
    let mut observed = DeploymentObservedState {
        deployment_id: "dep-scale".into(),
        target_backend: "runpod".into(),
        model_id: "qwen-72b".into(),
        active_replicas: 1,
        healthy_replicas: 1,
        phase: DeploymentPhase::Healthy,
        last_operation_id: None,
        observed_generation: 1,
    };

    let up = reconciler.reconcile(&desired, &mut observed);
    assert_eq!(up.action, ReconcileAction::ScaleUp);
    assert_eq!(observed.active_replicas, 3);
    assert_eq!(observed.healthy_replicas, 3);

    observed.active_replicas = 8;
    observed.healthy_replicas = 8;
    let down = reconciler.reconcile(&desired, &mut observed);
    assert_eq!(down.action, ReconcileAction::ScaleDown);
    assert_eq!(observed.active_replicas, 5);
    assert_eq!(observed.healthy_replicas, 5);
}

#[test]
fn vc_201_055_rollback_without_known_good_generation_returns_err() {
    let mut reconciler = CloudReconciler::new();
    let mut desired = DeploymentDesiredState {
        deployment_id: "dep-no-history".into(),
        target_backend: "runpod".into(),
        model_id: "qwen-72b".into(),
        min_replicas: 1,
        max_replicas: 2,
        gpu_type: "A100".into(),
        generation: 1,
    };
    let mut observed = DeploymentObservedState {
        deployment_id: "dep-no-history".into(),
        target_backend: "runpod".into(),
        model_id: "qwen-72b".into(),
        active_replicas: 0,
        healthy_replicas: 0,
        phase: DeploymentPhase::Pending,
        last_operation_id: None,
        observed_generation: 0,
    };
    reconciler.reconcile(&desired, &mut observed);
    reconciler.run_health_check(&desired, &mut observed, false);
    assert!(reconciler.rollback(&mut desired, &mut observed).is_err());
}

#[test]
fn vc_201_055_compute_operation_id_is_deterministic_and_sensitive_to_inputs() {
    let id1 =
        CloudReconciler::compute_operation_id("dep-a", 1, "runpod", ReconcileAction::Provision);
    let id2 =
        CloudReconciler::compute_operation_id("dep-a", 1, "runpod", ReconcileAction::Provision);
    assert_eq!(id1, id2);

    let diff_gen =
        CloudReconciler::compute_operation_id("dep-a", 2, "runpod", ReconcileAction::Provision);
    let diff_backend =
        CloudReconciler::compute_operation_id("dep-a", 1, "modal", ReconcileAction::Provision);
    let diff_action =
        CloudReconciler::compute_operation_id("dep-a", 1, "runpod", ReconcileAction::ScaleUp);
    assert_ne!(id1, diff_gen);
    assert_ne!(id1, diff_backend);
    assert_ne!(id1, diff_action);
}

#[test]
fn vc_201_055_health_check_transitions_between_healthy_and_degraded() {
    let mut reconciler = CloudReconciler::new();
    let desired = DeploymentDesiredState {
        deployment_id: "dep-health".into(),
        target_backend: "runpod".into(),
        model_id: "qwen-72b".into(),
        min_replicas: 2,
        max_replicas: 4,
        gpu_type: "A100".into(),
        generation: 1,
    };
    let mut observed = DeploymentObservedState {
        deployment_id: "dep-health".into(),
        target_backend: "runpod".into(),
        model_id: "qwen-72b".into(),
        active_replicas: 2,
        healthy_replicas: 2,
        phase: DeploymentPhase::Healthy,
        last_operation_id: None,
        observed_generation: 1,
    };

    reconciler.run_health_check(&desired, &mut observed, false);
    assert_eq!(observed.phase, DeploymentPhase::Degraded);
    assert_eq!(observed.healthy_replicas, 0);

    reconciler.run_health_check(&desired, &mut observed, true);
    assert_eq!(observed.phase, DeploymentPhase::Healthy);
    assert_eq!(observed.healthy_replicas, 2);
}
