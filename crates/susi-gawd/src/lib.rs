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

extern crate self as susi_gawd_a2a;
extern crate self as susi_gawd_agents;
extern crate self as susi_gawd_swarm;

pub mod susi_abi;

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
        pub use crate::swarm_ama::*;
    }
}

// Host / governance / evolution
// Vendored `susi-error` contract + IPC reporter: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
pub mod susi_error;

// Vendored `susi-paths` IPC client: full surface kept identical
// across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
mod susi_paths;

// Vendored `susi-config` surface + IPC client: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
// rustfmt::skip: the file is vendored byte-identical while consumers span
// edition 2021/2024 whose style editions sort imports and indent format!
// args differently — formatting it per-crate would break the invariant.
#[allow(dead_code)]
#[rustfmt::skip]
pub mod susi_config;

// Vendored `susi-sandbox` surface + IPC client: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
#[rustfmt::skip]
pub mod susi_sandbox;

// Vendored `susi-native` surface + IPC client: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
#[rustfmt::skip]
pub mod susi_native;

// Vendored `susi_core` microkernel subset (canonical tree:
// `susi-core/vendor_template/susi_core/`): bus/registry/capture/mac state
// rendezvous with the daemon's real susi_core via `<cache>/bus/<pid>/` +
// substrate files. Allows keep the tree byte-identical across consumers:
// dead_code audits the unexercised surface; rustfmt::skip + collapsible_if
// stop edition-2024 style drift against the edition-2021 canonical source.
#[allow(dead_code, clippy::collapsible_if)]
#[rustfmt::skip]
pub mod susi_core;

pub mod plane_handler;

#[path = "../../susi-gawd-agents/src/accountability.rs"]
pub mod accountability;
#[path = "../../susi-gawd-agents/src/admin_hooks.rs"]
pub mod admin_hooks;
#[path = "../../susi-gawd-agents/src/agents/mod.rs"]
pub mod agents;
#[path = "../../susi-gawd-agents/src/axiom.rs"]
pub mod axiom;
#[path = "../../susi-gawd-agents/src/brain.rs"]
pub mod brain;
#[path = "../../susi-gawd-agents/src/dag_hooks.rs"]
pub mod dag_hooks;
#[path = "../../susi-gawd-agents/src/external_peers.rs"]
pub mod external_peers;
#[path = "../../susi-gawd-agents/src/goal_shape.rs"]
pub mod goal_shape;
#[path = "../../susi-gawd-agents/src/live_search.rs"]
pub mod live_search;
#[path = "../../susi-gawd-agents/src/pkb.rs"]
pub mod pkb;
#[path = "../../susi-gawd-agents/src/safety.rs"]
pub mod safety;
#[path = "../../susi-gawd-agents/src/scheduler.rs"]
pub mod scheduler;
#[path = "../../susi-gawd-agents/src/security.rs"]
pub mod security;
#[path = "../../susi-gawd-agents/src/self_core.rs"]
pub mod self_core;
#[path = "../../susi-gawd-agents/src/system_observe.rs"]
pub mod system_observe;
#[cfg(test)]
#[path = "../../susi-gawd-agents/src/test_plane.rs"]
pub(crate) mod test_plane;

#[path = "../../susi-gawd-swarm/src/amas.rs"]
pub mod amas;
#[path = "../../susi-gawd-swarm/src/cloud_recovery.rs"]
pub(crate) mod cloud_recovery;
#[path = "../../susi-gawd-swarm/src/dag.rs"]
pub mod dag;
#[path = "../../susi-gawd-swarm/src/host_hooks.rs"]
pub mod host_hooks;
#[path = "../../susi-gawd-swarm/src/peer_registry.rs"]
pub mod peer_registry;
#[path = "../../susi-gawd-swarm/src/ama/mod.rs"]
mod swarm_ama;

#[path = "../../susi-gawd-a2a/src/capabilities.rs"]
pub mod capabilities;
#[path = "../../susi-gawd-a2a/src/executor.rs"]
pub mod executor;
#[path = "../../susi-gawd-a2a/src/server.rs"]
pub mod server;
#[path = "../../susi-gawd-a2a/src/task_store.rs"]
pub mod task_store;

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
pub use agents::{GawdAgentFleet, GawdAgentInfo, HighDensityContextStore};
pub use axiom::AxiomSubstrate;
pub use brain::AlphaBrainContext;

/// Compatibility `ama` facade: wires host hooks on [`SusiMasterAgent::new`].
pub mod ama {
    use super::init_hooks;
    use crate::susi_error::EaiResult;

    pub use crate::swarm_ama::{SusiMissionReport, SusiSwarmReport};

    /// Host-facing AMA constructor. Returns the swarm agent after wiring hooks.
    pub struct SusiMasterAgent;

    impl SusiMasterAgent {
        /// Returns the swarm AMA after wiring host hooks (compat with prior unit-struct API).
        #[allow(clippy::new_ret_no_self)]
        pub fn new() -> crate::swarm_ama::SusiMasterAgent {
            init_hooks();
            crate::swarm_ama::SusiMasterAgent::new()
        }

        pub fn sanitize_input(input: &str) -> EaiResult<String> {
            init_hooks();
            crate::swarm_ama::SusiMasterAgent::sanitize_input(input)
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
