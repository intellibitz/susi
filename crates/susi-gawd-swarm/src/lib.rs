#![deny(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        unsafe_code,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! GAWD **swarm** tier: AMA / AMAS orchestration, MissionDag, cloud recovery.
//!
//! Depends on [`susi_gawd_agents`] only. Must not import `ra2a` or `susi-gawd`.

pub use susi_error;

pub use susi_config;

pub use susi_sandbox_client as susi_sandbox;

pub use susi_core;

pub mod agent_integration;
pub mod agent_routing;
pub mod ama;
pub mod amas;
pub mod anti_entropy;
pub mod audit;
pub mod brain_supervisor;
pub mod cancel_propagate;
pub mod capacity_admission;
pub mod chaos_deployment;
pub mod cloud_failover;
pub mod cloud_lockout;
pub(crate) mod cloud_recovery;
pub mod dag;
pub mod deliberation;
pub mod fair_queue;
pub mod fault_matrix;
pub mod host_hooks;
pub mod identity_revoke;
pub mod independent_verify;
pub mod joint_consensus;
pub mod mission_persist;
pub mod mission_resume;
pub mod node_enrollment;
pub mod parallel_admission;
pub mod parallel_dispatch;
pub mod peer_registry;
pub mod quorum_durable;
pub mod resource_schedule;
pub mod roadmap_agents;
pub mod role_select;
pub mod side_effect_journal;
pub mod side_effects;
pub mod swarm_chaos;
pub mod task_lease;
pub mod worker_recovery;
pub mod writer_isolation;

pub use susi_gawd_agents::{
    accountability, admin_hooks, agents, axiom, brain, dag_hooks, external_peers, goal_shape, pkb,
    safety, scheduler, security, self_core, system_observe,
};

pub use agents::{GawdAgentFleet, GawdAgentInfo, HighDensityContextStore};
pub use axiom::AxiomSubstrate;
pub use brain::AlphaBrainContext;
pub use self_core::AlphaSelf;

pub use ama::SusiMasterAgent;
pub use dag::{MissionDag, SwarmDag};

#[cfg(test)]
#[path = "tests/audit_action_choke_point.rs"]
mod audit_action_choke_point_tests;
#[cfg(test)]
#[path = "tests/audit_payload_free_policy.rs"]
mod audit_payload_free_policy_tests;
#[cfg(test)]
#[path = "tests/cancel_propagate_dispatch_wiring.rs"]
mod cancel_propagate_dispatch_wiring_tests;
#[cfg(test)]
#[path = "tests/cancel_terminates.rs"]
mod cancel_terminates_tests;
#[cfg(test)]
#[path = "tests/durable_fence.rs"]
mod durable_fence_tests;
#[cfg(test)]
#[path = "tests/durable_mission_state.rs"]
mod durable_mission_state_tests;
#[cfg(test)]
#[path = "tests/fair_queue_dispatch_wiring.rs"]
mod fair_queue_dispatch_wiring_tests;
#[cfg(test)]
#[path = "tests/independent_verify_dispatch_wiring.rs"]
mod independent_verify_dispatch_wiring_tests;
#[cfg(test)]
#[path = "tests/joint_consensus_membership_wiring.rs"]
mod joint_consensus_membership_wiring_tests;
#[cfg(test)]
#[path = "tests/mission_dag_persist_wiring.rs"]
mod mission_dag_persist_wiring_tests;
#[cfg(test)]
#[path = "tests/mission_resume_cli_wiring.rs"]
mod mission_resume_cli_wiring_tests;
#[cfg(test)]
#[path = "tests/native_role_select_wiring.rs"]
mod native_role_select_wiring_tests;
#[cfg(test)]
#[path = "tests/parallel_roadmap_e2e.rs"]
mod parallel_roadmap_e2e_tests;
#[cfg(test)]
#[cfg(test)]
#[path = "tests/resource_schedule_dispatch_wiring.rs"]
mod resource_schedule_dispatch_wiring_tests;
#[cfg(test)]
#[path = "tests/side_effect_journal.rs"]
mod side_effect_journal_tests;
#[cfg(test)]
#[path = "tests/side_effects_dispatch_wiring.rs"]
mod side_effects_dispatch_wiring_tests;
#[cfg(test)]
#[path = "tests/snapshot_rollback.rs"]
mod snapshot_rollback_tests;
#[cfg(test)]
#[path = "tests/swarm_chaos.rs"]
mod swarm_chaos_tests;
#[cfg(test)]
#[path = "tests/swarm_gap_durable_fencing.rs"]
mod swarm_gap_durable_fencing_tests;
#[cfg(test)]
#[path = "tests/swarm_gap_live_admission.rs"]
mod swarm_gap_live_admission_tests;
#[cfg(test)]
#[path = "tests/swarm_gap_real_cancellation.rs"]
mod swarm_gap_real_cancellation_tests;
#[cfg(test)]
#[path = "tests/swarm_gap_safe_replay.rs"]
mod swarm_gap_safe_replay_tests;
#[cfg(test)]
#[path = "tests/swarm_gap_verified_closure.rs"]
mod swarm_gap_verified_closure_tests;
#[cfg(test)]
#[path = "tests/swarm_gap_workspace_isolation.rs"]
mod swarm_gap_workspace_isolation_tests;
#[cfg(test)]
#[path = "tests/task_lease_dispatch_wiring.rs"]
mod task_lease_dispatch_wiring_tests;
#[cfg(test)]
#[path = "tests/vc_201_021.rs"]
mod vc_201_021_tests;
#[cfg(test)]
#[path = "tests/vc_201_022.rs"]
mod vc_201_022_tests;
#[cfg(test)]
#[path = "tests/vc_201_023.rs"]
mod vc_201_023_tests;
#[cfg(test)]
#[path = "tests/vc_201_024.rs"]
mod vc_201_024_tests;
#[cfg(test)]
#[path = "tests/vc_201_025.rs"]
mod vc_201_025_tests;
#[cfg(test)]
#[path = "tests/vc_201_026.rs"]
mod vc_201_026_tests;
#[cfg(test)]
#[path = "tests/vc_201_027.rs"]
mod vc_201_027_tests;
#[cfg(test)]
#[path = "tests/vc_201_028.rs"]
mod vc_201_028_tests;
#[cfg(test)]
#[path = "tests/vc_201_030.rs"]
mod vc_201_030_tests;
#[cfg(test)]
#[path = "tests/vc_201_032.rs"]
mod vc_201_032_tests;
#[cfg(test)]
#[path = "tests/vc_201_033.rs"]
mod vc_201_033_tests;
#[cfg(test)]
#[path = "tests/vc_201_037.rs"]
mod vc_201_037_tests;
#[cfg(test)]
#[path = "tests/vc_201_040.rs"]
mod vc_201_040_tests;
#[cfg(test)]
#[path = "tests/vc_201_043_mastery.rs"]
mod vc_201_043_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_052_mastery.rs"]
mod vc_201_052_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_095_mastery.rs"]
mod vc_201_095_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_202_007_mastery.rs"]
mod vc_202_007_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_202_009_mastery.rs"]
mod vc_202_009_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_202_014_mastery.rs"]
mod vc_202_014_mastery_tests;
#[cfg(test)]

/// Wire the MissionDag post-swarm hook into the agents leaf.
///
/// Call once from the host (`susi-gawd`) at load — idempotent first-wins.
pub fn init() {
    susi_gawd_agents::dag_hooks::init(dag::dispatch_mission_dag);
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn init_wires_mission_dag_hook_idempotently() {
        init();
        init();
        let sanitized = SusiMasterAgent::sanitize_input("verify swarm status")
            .expect("benign input should sanitize");
        assert!(sanitized.contains("verify swarm status"));
    }

    #[test]
    fn fault_matrix_run_matrix_passes_all_scenarios_via_swarm_root() {
        let report = fault_matrix::run_matrix();
        assert!(report.all_passed());
        assert!(!report.scenarios.is_empty());
    }

    #[test]
    fn fault_kind_serde_roundtrip_preserves_variant() {
        let kind = fault_matrix::FaultKind::Partition;
        let json = serde_json::to_string(&kind).expect("serialize");
        let decoded: fault_matrix::FaultKind = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, kind);
    }

    #[test]
    fn susi_error_reexport_constructs_process_error_from_swarm_root() {
        let err = susi_error::EaiError::process("swarm root test");
        assert!(err.to_string().contains("swarm root test"));
    }

    #[test]
    fn joint_consensus_module_computes_electorate_quorum_via_swarm_root() {
        let electorate = joint_consensus::Electorate::from_ids(["n1", "n2", "n3"]);
        assert_eq!(electorate.quorum_size(), 2);
    }
}
