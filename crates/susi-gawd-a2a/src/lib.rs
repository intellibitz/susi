//! GAWD **A2A** tier: Agent2Agent wire protocol (`ra2a`).
//!
//! Depends on [`susi_gawd_agents`] only (`GawdAgentFleet`). Must not depend on
//! swarm or the host `susi-gawd` crate. `handler.rs` is kept on disk but left
//! out of the module tree (broken / unused).

pub mod capabilities;
pub mod executor;
pub mod task_store;

pub use capabilities::GawdCapabilities;
pub use executor::GawdA2AExecutor;
pub use task_store::GawdTaskStore;
