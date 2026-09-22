//! GAWD **swarm** tier: AMA / AMAS orchestration, MissionDag, cloud recovery.
//!
//! Depends on [`susi_gawd_agents`] only. Must not import `ra2a` or `susi-gawd`.

pub mod ama;
pub mod amas;
pub(crate) mod cloud_recovery;
pub mod dag;
pub mod host_hooks;
pub mod peer_registry;

pub use ama::SusiMasterAgent;
pub use dag::{MissionDag, SwarmDag};

/// Wire the MissionDag post-swarm hook into the agents leaf.
///
/// Call once from the host (`susi-gawd`) at load — idempotent first-wins.
pub fn init() {
    susi_gawd_agents::dag_hooks::init(dag::dispatch_mission_dag);
}
