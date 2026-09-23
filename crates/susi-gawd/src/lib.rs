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
//! | **Host** | this crate (`0.5.0`) | Admin, evolution, compliance, facade re-exports |
//!
//! DAG: agents ← swarm, agents ← a2a, all three ← gawd. No cycles.
//! Flat paths (`agents`, `ama`, `amas`, …) remain as compatibility re-exports.

use std::path::Path;
use std::sync::Once;
use susi_error::{EaiError, EaiResult};
use susi_gawd_agents::admin_hooks::AdminHooks;
use susi_gawd_swarm::host_hooks::HostHooks;

pub use susi_gawd_a2a as a2a;
pub use susi_gawd_swarm as swarm;

// Host / governance / evolution
pub mod plane_handler;

pub mod admin;
pub mod bloat_audit;
pub mod compliance;
pub mod evolution;
pub mod genome_distiller;
pub mod kernel_loader;
pub mod patch_cycle;
pub mod reason_trainer;
pub mod reflex_synth;
pub mod reflex_trainer;
pub mod self_validation;

// Tier surfaces kept on the host public API
pub use susi_gawd_agents::{accountability, axiom, brain, safety, security, self_core};
pub use susi_gawd_agents::{external_peers, pkb};

// ── Flat compatibility re-exports (do not remove without a migration) ─────
pub use susi_gawd_a2a::{capabilities, executor, task_store};
pub use susi_gawd_agents::agents;
pub use susi_gawd_swarm::{amas, dag};

/// Compatibility `ama` facade: wires host hooks on [`SusiMasterAgent::new`].
pub mod ama {
    use super::init_hooks;
    use susi_error::EaiResult;

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

pub use susi_core::{bus, capture, evidence, manifold, queue, truth};
pub use susi_core::{net_guard, task_manager};

pub use ama::SusiMasterAgent;
pub use susi_gawd_agents::AlphaSelf;

struct GawdAdminHooks;
impl AdminHooks for GawdAdminHooks {
    fn enforce_version_consistency(&self, workspace: &Path) -> EaiResult<String> {
        admin::SusiAdmin::enforce_version_consistency(workspace)
    }
    fn audit_compliance(&self, workspace: &Path) -> EaiResult<String> {
        admin::SusiAdmin::audit_compliance(workspace, None)
    }
    fn verify_version_alignment(&self, workspace: &Path) -> EaiResult<String> {
        admin::SusiAdmin::verify_version_alignment(workspace)
            .map(|_| "Version alignment verified.".to_string())
    }
    fn execute_release(&self, workspace: &Path) -> EaiResult<String> {
        admin::SusiAdmin::execute_release(workspace, None)
    }
    fn bloat_audit_workspace(&self, workspace: &Path) -> EaiResult<String> {
        let report = bloat_audit::BloatAuditor::audit_workspace(workspace)?;
        Ok(bloat_audit::BloatAuditor::render_report(&report))
    }
    fn perform_autonomous_drift_audit(&self, workspace: &Path) -> EaiResult<String> {
        evolution::EvolutionManager::perform_autonomous_drift_audit(workspace)
    }
    fn apply_patch_cycle(
        &self,
        workspace: &Path,
        request_json: &str,
        trust_level: &str,
    ) -> EaiResult<String> {
        let request: patch_cycle::PatchRequest = serde_json::from_str(request_json)
            .map_err(|e| EaiError::governance(format!("invalid patch request JSON: {e}")))?;
        let outcome = patch_cycle::apply_patch_cycle(workspace, &request, trust_level)?;
        serde_json::to_string_pretty(&outcome)
            .map_err(|e| EaiError::governance(format!("serialize patch outcome: {e}")))
    }
}

struct GawdHostHooks;
impl HostHooks for GawdHostHooks {
    fn audit_distillation_state(&self, workspace: &Path) -> EaiResult<String> {
        reflex_trainer::ReflexTrainer::audit_distillation_state(workspace)
    }
}

static HOOKS_ONCE: Once = Once::new();

/// Wire admin + distillation + MissionDag hooks. Idempotent (first-wins).
pub fn init_hooks() {
    HOOKS_ONCE.call_once(|| {
        susi_gawd_swarm::init();
        susi_gawd_agents::admin_hooks::init(Box::new(GawdAdminHooks));
        susi_gawd_swarm::host_hooks::init(Box::new(GawdHostHooks));
    });
}
