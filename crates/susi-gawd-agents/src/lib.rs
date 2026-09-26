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

//! GAWD **agents** tier: fleet primitives, native agents, peers, governance detectors.
//!
//! Leaf crate in the GAWD DAG — must not depend on `susi-gawd-swarm`,
//! `susi-gawd-a2a`, or `susi-gawd`. Host admin actions and MissionDag execution
//! reach this crate only through [`admin_hooks`] / [`dag_hooks`].

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

// The vendored `susi_core` re-exports this crate's `susi_error` module
// (`susi_core::susi_error` is `crate::susi_error`), so no `From` bridge is
// needed — `?` converts trivially.

pub mod accountability;
pub mod admin_hooks;
pub mod agents;
pub mod axiom;
pub mod brain;
pub mod dag_hooks;
pub mod external_peers;
pub mod goal_shape;
pub(crate) mod live_search;
pub mod pkb;
pub mod safety;
pub mod scheduler;
pub mod security;
pub mod self_core;
pub mod system_observe;

pub use agents::{
    AgentMetaRegistry, AgentProfile, DiscoverableAsset, GawdAgent, GawdAgentFleet, GawdAgentInfo,
    HighDensityContextStore, MissionBlackboard, SwarmBlackboard,
};
pub use axiom::AxiomSubstrate;
pub use brain::AlphaBrainContext;
pub use self_core::AlphaSelf;

/// In-memory `agents.` plane handler for unit tests. The concrete meta
/// registry lives in `susi-agents` — a crate this leaf cannot depend on — so
/// without this stub `AgentMetaRegistry` facade calls hit an unwired bus and
/// silently no-op. Seeded with the single profile the scheduler tests need;
/// `is_core` is deliberately false so the seed never auto-recruits itself
/// into `synthesize_fleet` results for unrelated goals.
#[cfg(test)]
pub(crate) mod test_plane;
