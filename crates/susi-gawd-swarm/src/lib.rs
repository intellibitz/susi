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

extern crate self as susi_gawd_agents;

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

// Vendored `susi_core` microkernel subset (canonical tree:
// `susi-core/vendor_template/susi_core/`): bus/registry/capture/mac state
// rendezvous with the daemon's real susi_core via `<cache>/bus/<pid>/` +
// substrate files. Allows keep the tree byte-identical across consumers:
// dead_code audits the unexercised surface; rustfmt::skip + collapsible_if
// stop edition-2024 style drift against the edition-2021 canonical source.
#[allow(dead_code, clippy::collapsible_if)]
#[rustfmt::skip]
pub mod susi_core;

pub mod ama;
pub mod amas;
pub(crate) mod cloud_recovery;
pub mod dag;
pub mod host_hooks;
pub mod peer_registry;

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

pub use agents::{GawdAgentFleet, GawdAgentInfo, HighDensityContextStore};
pub use axiom::AxiomSubstrate;
pub use brain::AlphaBrainContext;
pub use self_core::AlphaSelf;

pub use ama::SusiMasterAgent;
pub use dag::{MissionDag, SwarmDag};

/// Wire the MissionDag post-swarm hook into the agents leaf.
///
/// Call once from the host (`susi-gawd`) at load — idempotent first-wins.
pub fn init() {
    susi_gawd_agents::dag_hooks::init(dag::dispatch_mission_dag);
}
