//! Vector VC-201-055 mastery tests.
//!
//! Vector: Reconcile managed cloud inference deployments.
//! Mastery target: Extend the existing provisioning path with desired/observed
//! state, stable operation IDs, health checks, and rollback for an explicitly
//! supported backend; repeated apply and crash recovery do not create duplicate resources.

use crate::cloud_reconciler::{
    CloudReconciler, DeploymentDesiredState, DeploymentObservedState, DeploymentPhase,
    ReconcileAction,
};
use crate::eco_gpu_clouds::{profile, PROFILE_ID};

#[test]
fn vc_201_055_mastery_catalog_profile_loads() {
    assert_eq!(PROFILE_ID, "gpu-clouds");
    let p = profile().expect("gpu-clouds profile should load from bundled assets");
    assert!(!p.capabilities.is_empty());
}

#[test]
fn vc_201_055_mastery_reconciliation_engine_implemented() {
    let mut reconciler = CloudReconciler::new();
    let desired = DeploymentDesiredState {
        deployment_id: "dep-mastery-runpod".into(),
        target_backend: "runpod".into(),
        model_id: "llama3-70b".into(),
        min_replicas: 1,
        max_replicas: 3,
        gpu_type: "A100".into(),
        generation: 1,
    };
    let mut observed = DeploymentObservedState {
        deployment_id: "dep-mastery-runpod".into(),
        target_backend: "runpod".into(),
        model_id: "llama3-70b".into(),
        active_replicas: 0,
        healthy_replicas: 0,
        phase: DeploymentPhase::Pending,
        last_operation_id: None,
        observed_generation: 0,
    };

    let outcome = reconciler.reconcile(&desired, &mut observed);
    assert_eq!(outcome.action, ReconcileAction::Provision);
    assert_eq!(observed.phase, DeploymentPhase::Healthy);
    assert_eq!(observed.observed_generation, 1);
}

#[test]
fn vc_201_055_mastery_crash_recovery_deduplication() {
    let mut reconciler = CloudReconciler::new();
    let desired = DeploymentDesiredState {
        deployment_id: "dep-mastery-dedup".into(),
        target_backend: "modal".into(),
        model_id: "mistral-7b".into(),
        min_replicas: 2,
        max_replicas: 4,
        gpu_type: "T4".into(),
        generation: 1,
    };
    let mut observed = DeploymentObservedState {
        deployment_id: "dep-mastery-dedup".into(),
        target_backend: "modal".into(),
        model_id: "mistral-7b".into(),
        active_replicas: 0,
        healthy_replicas: 0,
        phase: DeploymentPhase::Pending,
        last_operation_id: None,
        observed_generation: 0,
    };

    let outcome1 = reconciler.reconcile(&desired, &mut observed);
    assert!(!outcome1.deduplicated);

    // Simulate crash before observed state was committed (replayed from pre-crash snapshot)
    let mut recovered_observed = DeploymentObservedState {
        deployment_id: "dep-mastery-dedup".into(),
        target_backend: "modal".into(),
        model_id: "mistral-7b".into(),
        active_replicas: 0,
        healthy_replicas: 0,
        phase: DeploymentPhase::Pending,
        last_operation_id: None,
        observed_generation: 0,
    };
    let outcome2 = reconciler.reconcile(&desired, &mut recovered_observed);
    assert!(outcome2.deduplicated);
    assert_eq!(outcome1.operation_id, outcome2.operation_id);
}
