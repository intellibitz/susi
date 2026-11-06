use crate::reconcile::{
    reconcile_step, stable_operation_id, DesiredDeployment, ManagedCloudBackend,
    ObservedDeployment, ReconcileAction,
};

fn desired(backend: ManagedCloudBackend, revision: &str, replicas: u32) -> DesiredDeployment {
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
            reason: "health check failed; rolled back to last known good revision".to_string(),
        }
    );
    assert_eq!(
        observed.active_revision.as_deref(),
        Some("rev-1"),
        "failed rollout must restore active_revision to rev-1"
    );
    assert!(observed.healthy);
}
