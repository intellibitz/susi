#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! VC-201-068: Roll configuration changes across a fleet in stages.
//!
//! Mastery criteria:
//! 1. Bounded waves: canary wave (wave 0) must be completed and healthy before later waves open.
//!    Attempting to apply later-wave nodes while earlier waves are in flight is strictly rejected.
//! 2. Unhealthy node pauses rollout: reporting unhealthy immediately pauses rollout and empties pending.
//! 3. Verified-health rollback requirement: rollback restores pre-rollout revision but clears verified health;
//!    calling resume before fresh health verification is strictly rejected.
//! 4. Non-empty canary requirement: non-empty fleets require at least one canary node.
//! 5. FleetRolloutDriver automated execution: driver advances waves sequentially and completes when all nodes
//!    are pinned and healthy.

use susi_config::fleet_rollout::{FleetRolloutDriver, Rollout, RolloutStatus};

fn sample_fleet() -> Vec<(String, Option<String>)> {
    vec![
        ("node-canary".into(), Some("v1.0.0".into())),
        ("node-worker-1".into(), Some("v1.0.0".into())),
        ("node-worker-2".into(), Some("v1.0.0".into())),
        ("node-worker-3".into(), Some("v1.0.0".into())),
        ("node-worker-4".into(), Some("v1.0.0".into())),
    ]
}

#[test]
fn vc_201_068_mastery() {
    let fleet = sample_fleet();
    let canary = vec!["node-canary".to_string()];

    // 1. Non-empty fleet requires at least one canary node
    let no_canary = Rollout::new("v2.0.0", &fleet, &[], 2);
    assert!(
        no_canary.is_err(),
        "Rollout must reject empty canary on non-empty fleet"
    );

    // 2. Initialize staged rollout: 1 canary (wave 0), wave 1 (worker-1, worker-2), wave 2 (worker-3, worker-4)
    let mut driver = FleetRolloutDriver::new("v2.0.0", &fleet, &canary, 2)
        .expect("driver initialization succeeds");

    assert_eq!(driver.rollout.open_wave, 0);
    assert_eq!(driver.rollout.pending(), vec!["node-canary".to_string()]);

    // 3. Strict wave boundary enforcement: worker-1 (wave 1) cannot be applied while wave 0 is active
    let premature_apply = driver.rollout.mark_applied("node-worker-1", "v2.0.0");
    assert!(
        premature_apply.is_err(),
        "Applying later wave node before open wave is done must fail: {premature_apply:?}"
    );

    // 4. Test failure handling and verified-health rollback requirement
    // Apply canary
    driver
        .rollout
        .mark_applied("node-canary", "v2.0.0")
        .expect("apply canary");
    // Canary fails health probe
    driver
        .rollout
        .report("node-canary", false)
        .expect("report unhealthy");
    assert_eq!(driver.rollout.status, RolloutStatus::Paused);
    assert!(
        driver.rollout.pending().is_empty(),
        "pending() must be empty while paused"
    );

    // Rollback canary to pre-rollout revision
    let rollback_prev = driver
        .rollout
        .rollback("node-canary")
        .expect("rollback canary");
    assert_eq!(rollback_prev, Some("v1.0.0".into()));

    // Rollback does not fabricate health; calling resume before health verification fails
    let premature_resume = driver.rollout.resume();
    assert!(
        premature_resume.is_err(),
        "resume must be rejected until fresh health verification is recorded: {premature_resume:?}"
    );

    // Record verified health on rolled back canary
    driver
        .rollout
        .report("node-canary", true)
        .expect("report healthy on rollback");
    driver
        .rollout
        .resume()
        .expect("resume succeeds after verification");
    assert_eq!(driver.rollout.status, RolloutStatus::Running);

    // 5. End-to-end multi-wave execution via FleetRolloutDriver
    // Reset driver for clean full staged rollout
    let mut active_driver =
        FleetRolloutDriver::new("v2.0.0", &fleet, &canary, 2).expect("active driver created");

    // Wave 0: Canary
    let wave0_nodes = active_driver
        .step(|_node, rev| Ok(rev == "v2.0.0"))
        .expect("step wave 0");
    assert_eq!(wave0_nodes, vec!["node-canary".to_string()]);
    assert_eq!(
        active_driver.rollout.nodes["node-canary"]
            .current
            .as_deref(),
        Some("v2.0.0")
    );
    assert!(active_driver.rollout.nodes["node-canary"].healthy);
    assert_eq!(active_driver.rollout.open_wave, 1);

    // Wave 1: Workers 1 & 2
    let wave1_nodes = active_driver
        .step(|_node, rev| Ok(rev == "v2.0.0"))
        .expect("step wave 1");
    assert_eq!(
        wave1_nodes,
        vec!["node-worker-1".to_string(), "node-worker-2".to_string()]
    );
    assert_eq!(active_driver.rollout.open_wave, 2);

    // Wave 2: Workers 3 & 4
    let wave2_nodes = active_driver
        .step(|_node, rev| Ok(rev == "v2.0.0"))
        .expect("step wave 2");
    assert_eq!(
        wave2_nodes,
        vec!["node-worker-3".to_string(), "node-worker-4".to_string()]
    );

    // 6. Complete status check
    assert_eq!(active_driver.rollout.status, RolloutStatus::Done);
    assert!(active_driver.rollout.all_pinned_healthy());
    assert!(active_driver.rollout.pending().is_empty());

    // 7. Empty fleet initialization completes immediately
    let empty_driver =
        FleetRolloutDriver::new("v2.0.0", &[], &[], 2).expect("empty driver created");
    assert_eq!(empty_driver.rollout.status, RolloutStatus::Done);
    assert!(empty_driver.rollout.pending().is_empty());

    // 8. Applier callback error does not strand the node
    let mut err_driver =
        FleetRolloutDriver::new("v2.0.0", &fleet, &canary, 2).expect("err driver created");
    let err_result = err_driver.step(|_node, _rev| {
        Err(susi_config::susi_error::EaiError::config(
            "simulated network error",
        ))
    });
    assert!(err_result.is_err());
    assert_eq!(err_driver.rollout.status, RolloutStatus::Paused);
    // Canary was NOT stranded with target revision
    assert_eq!(
        err_driver.rollout.nodes["node-canary"].current.as_deref(),
        Some("v1.0.0"),
        "node must retain its previous revision when applier fails"
    );
    err_driver.rollout.status = RolloutStatus::Running;
    // Retrying succeeds
    let retry_nodes = err_driver
        .step(|_node, rev| Ok(rev == "v2.0.0"))
        .expect("retry step succeeds");
    assert_eq!(retry_nodes, vec!["node-canary".to_string()]);
}
