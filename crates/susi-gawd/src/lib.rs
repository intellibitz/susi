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

//! GAWD — Swarm Master Authority.
//!
//! # Four-crate layout
//!
//! | Tier | Crate | Responsibility |
//! |------|-------|----------------|
//! | **Agents** | [`susi_gawd_agents`] | Fleet, peers, detectors, identity tables |
//! | **Swarm** | [`susi_gawd_swarm`] | AMA / AMAS / DAG / cloud recovery |
//! | **A2A** | [`susi_gawd_a2a`] | `ra2a` wire protocol |
//! | **Host** | this crate (`0.5.0`) | Admin (incl. compliance audit), evolution, facade re-exports |
//!
//! DAG: agents ← swarm, agents ← a2a, all three ← gawd. No cycles.
//! Flat paths (`agents`, `ama`, `amas`, …) remain as compatibility re-exports.

pub use susi_abi;

use crate::admin_hooks::AdminHooks;
use crate::host_hooks::HostHooks;
use std::path::Path;
use std::sync::Once;

pub mod a2a {
    pub use crate::{capabilities, executor, server, task_store};
}

pub mod swarm {
    pub use crate::{amas, dag, host_hooks, peer_registry};

    pub mod ama {
        pub use susi_gawd_swarm::ama::*;
    }
}

pub use susi_error;

pub use susi_config;

pub use susi_sandbox_client as susi_sandbox;

// One compiled native-service client; Wasmer remains isolated in the service.
pub use susi_native_client as susi_native;

pub use susi_core;

pub mod plane_handler;

pub use susi_gawd_agents::{
    accountability, admin_hooks, agents, axiom, brain, dag_hooks, external_peers, goal_shape, pkb,
    safety, scheduler, security, self_core, system_observe,
};

pub use susi_gawd_a2a::{capabilities, executor, server, task_store};
pub use susi_gawd_swarm::{amas, dag, host_hooks, peer_registry};

pub mod admin;
pub mod audit_evidence;
pub mod autonomous_builder;
pub mod baseline_capture;
pub mod bloat_audit;
pub mod brain_gap_audit;
pub mod brain_gap_dedup;
pub mod brain_gap_e2e;
pub mod brain_gap_publish;
pub mod brain_gap_tasks;
pub mod brain_gap_triggers;
pub mod brain_status;
pub mod capacity_limits;
pub mod changelog_from_tasks;
pub mod cloud_brain_wiring;
pub mod cloud_rsi;
pub mod cloud_rsi_delivery;
pub mod cloud_rsi_e2e;
pub mod cloud_rsi_lifecycle;
pub mod cloud_rsi_outcomes;
pub mod dev_canary;
pub mod disaster_recovery;
pub mod eval_contamination;
pub mod eval_receipt;
pub mod eval_separation;
pub mod evolution;
pub mod experiment_budget;
pub mod experiment_lifecycle;
pub mod experiment_memory;
pub mod formal_invariants;
pub mod gap_priority;
pub mod genome_distiller;
pub mod intent_invariants;
pub mod kernel_loader;
pub mod lane_overlap_warning;
pub mod ops_slo;
pub mod paired_regression;
pub mod patch_cycle;
pub mod patch_fence;
pub mod perf_regression_gate;
pub mod reason_trainer;
pub mod reflex_intent;
pub mod reflex_revisions;
pub mod reflex_synth;
pub mod reflex_trainer;
pub mod reflex_verify;
pub mod release_qualify;
pub mod repo_gate;
pub mod rsi_comparisons;
pub mod rsi_corpus;
pub mod rsi_generations;
pub mod rsi_promotion;
pub mod scorecard;
pub mod self_validation;
pub mod task_edit_guard;
pub mod tasks_from_brain;
pub mod tasks_from_ci;
pub mod tenant_isolation;
pub mod unsafe_ratchet;
pub mod update_health;
pub mod zc_agent_files_gen;
pub mod zc_agent_identity;
pub mod zc_gh_auth_auto;
pub mod zc_task_scaffold;
pub mod zc_tasks_next;
pub mod zc_toolchain_auto;
pub mod zc_updates_default;
pub mod zc_worktree_gc;

#[cfg(test)]
#[path = "tests/formal_invariants.rs"]
mod formal_invariants_tests;
#[cfg(test)]
#[path = "tests/rsi_corpus.rs"]
mod rsi_corpus_tests;
#[cfg(test)]
#[path = "tests/vc_200_002.rs"]
mod vc_200_002_tests;
#[cfg(test)]
#[path = "tests/vc_201_001_mastery.rs"]
mod vc_201_001_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_002.rs"]
mod vc_201_002_tests;
#[cfg(test)]
#[path = "tests/vc_201_003_mastery.rs"]
mod vc_201_003_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_003.rs"]
mod vc_201_003_tests;
#[cfg(test)]
#[path = "tests/vc_201_004.rs"]
mod vc_201_004_tests;
#[cfg(test)]
#[path = "tests/vc_201_005_mastery.rs"]
mod vc_201_005_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_005.rs"]
mod vc_201_005_tests;
#[cfg(test)]
#[path = "tests/vc_201_006.rs"]
mod vc_201_006_tests;
#[cfg(test)]
#[path = "tests/vc_201_007_mastery.rs"]
mod vc_201_007_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_007.rs"]
mod vc_201_007_tests;
#[cfg(test)]
#[path = "tests/vc_201_008.rs"]
mod vc_201_008_tests;
#[cfg(test)]
#[path = "tests/vc_201_009.rs"]
mod vc_201_009_tests;
#[cfg(test)]
#[path = "tests/vc_201_010.rs"]
mod vc_201_010_tests;
#[cfg(test)]
#[path = "tests/vc_201_011.rs"]
mod vc_201_011_tests;
#[cfg(test)]
#[path = "tests/vc_201_012.rs"]
mod vc_201_012_tests;
#[cfg(test)]
#[path = "tests/vc_201_013.rs"]
mod vc_201_013_tests;
#[cfg(test)]
#[path = "tests/vc_201_014.rs"]
mod vc_201_014_tests;
#[cfg(test)]
#[path = "tests/vc_201_015.rs"]
mod vc_201_015_tests;
#[cfg(test)]
#[path = "tests/vc_201_016.rs"]
mod vc_201_016_tests;
#[cfg(test)]
#[path = "tests/vc_201_017.rs"]
mod vc_201_017_tests;
#[cfg(test)]
#[path = "tests/vc_201_018.rs"]
mod vc_201_018_tests;
#[cfg(test)]
#[path = "tests/vc_201_019.rs"]
mod vc_201_019_tests;
#[cfg(test)]
#[path = "tests/vc_201_020.rs"]
mod vc_201_020_tests;
#[cfg(test)]
#[path = "tests/vc_201_078.rs"]
mod vc_201_078_tests;
#[cfg(test)]
#[path = "tests/vc_201_079.rs"]
mod vc_201_079_tests;
#[cfg(test)]
#[path = "tests/vc_201_092.rs"]
mod vc_201_092_tests;
#[cfg(test)]
#[path = "tests/vc_201_093.rs"]
mod vc_201_093_tests;
#[cfg(test)]
#[path = "tests/vc_201_094.rs"]
mod vc_201_094_tests;
#[cfg(test)]
#[path = "tests/vc_201_097.rs"]
mod vc_201_097_tests;
#[cfg(test)]
#[path = "tests/vc_201_100.rs"]
mod vc_201_100_tests;

// Tier surfaces kept on the host public API
pub use agents::{GawdAgentFleet, GawdAgentInfo, HighDensityContextStore};
pub use axiom::AxiomSubstrate;
pub use brain::AlphaBrainContext;

/// Compatibility `ama` facade: wires host hooks on [`SusiMasterAgent::new`].
pub mod ama {
    use super::init_hooks;
    use crate::susi_error::EaiResult;

    pub use susi_gawd_swarm::ama::{SusiMissionReport, SusiSwarmReport};

    /// Host-facing AMA constructor. Returns the swarm agent after wiring hooks.
    pub struct SusiMasterAgent;

    impl SusiMasterAgent {
        /// Returns the swarm AMA after wiring host hooks (compat with prior unit-struct API).
        #[allow(clippy::new_ret_no_self)]
        pub fn new() -> susi_gawd_swarm::ama::SusiMasterAgent {
            init_hooks();
            susi_gawd_swarm::ama::SusiMasterAgent::new()
        }

        pub fn sanitize_input(input: &str) -> EaiResult<String> {
            init_hooks();
            susi_gawd_swarm::ama::SusiMasterAgent::sanitize_input(input)
        }
    }
}

pub use crate::susi_core::{bus, capture, evidence, manifold, queue, truth};
pub use crate::susi_core::{net_guard, task_manager};

pub use crate::self_core::AlphaSelf;
pub use ama::SusiMasterAgent;

struct GawdAdminHooks;
impl AdminHooks for GawdAdminHooks {
    fn enforce_version_consistency(
        &self,
        workspace: &Path,
    ) -> susi_gawd_agents::susi_error::EaiResult<String> {
        admin::SusiAdmin::enforce_version_consistency(workspace)
    }
    fn audit_compliance(
        &self,
        workspace: &Path,
    ) -> susi_gawd_agents::susi_error::EaiResult<String> {
        admin::SusiAdmin::audit_compliance(workspace, None)
    }
    fn verify_version_alignment(
        &self,
        workspace: &Path,
    ) -> susi_gawd_agents::susi_error::EaiResult<String> {
        admin::SusiAdmin::verify_version_alignment(workspace)
            .map(|_| "Version alignment verified.".to_string())
    }
    fn execute_release(&self, workspace: &Path) -> susi_gawd_agents::susi_error::EaiResult<String> {
        admin::SusiAdmin::execute_release(workspace, None)
    }
    fn bloat_audit_workspace(
        &self,
        workspace: &Path,
    ) -> susi_gawd_agents::susi_error::EaiResult<String> {
        let report = bloat_audit::BloatAuditor::audit_workspace(workspace)?;
        Ok(bloat_audit::BloatAuditor::render_report(&report))
    }
    fn perform_autonomous_drift_audit(
        &self,
        workspace: &Path,
    ) -> susi_gawd_agents::susi_error::EaiResult<String> {
        evolution::EvolutionManager::perform_autonomous_drift_audit(workspace)
    }
    fn apply_patch_cycle(
        &self,
        workspace: &Path,
        request_json: &str,
        trust_level: &str,
    ) -> susi_gawd_agents::susi_error::EaiResult<String> {
        let request: patch_cycle::PatchRequest =
            serde_json::from_str(request_json).map_err(|e| {
                susi_gawd_agents::susi_error::EaiError::governance(format!(
                    "invalid patch request JSON: {e}"
                ))
            })?;
        let outcome = patch_cycle::apply_patch_cycle(workspace, &request, trust_level)?;
        serde_json::to_string_pretty(&outcome).map_err(|e| {
            susi_gawd_agents::susi_error::EaiError::governance(format!(
                "serialize patch outcome: {e}"
            ))
        })
    }
}

struct GawdHostHooks;
impl HostHooks for GawdHostHooks {
    fn audit_distillation_state(
        &self,
        workspace: &Path,
    ) -> susi_gawd_swarm::susi_error::EaiResult<String> {
        reflex_trainer::ReflexTrainer::audit_distillation_state(workspace)
    }
}

static HOOKS_ONCE: Once = Once::new();

/// Wire admin + distillation + MissionDag hooks. Idempotent (first-wins).
pub fn init_hooks() {
    HOOKS_ONCE.call_once(|| {
        dag_hooks::init(dag::dispatch_mission_dag);
        susi_gawd_agents::admin_hooks::init(Box::new(GawdAdminHooks));
        susi_gawd_swarm::host_hooks::init(Box::new(GawdHostHooks));
    });
}
