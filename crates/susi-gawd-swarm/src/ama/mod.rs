//! Top-level mission orchestrator: sanitizes input, dispatches the swarm, and
//! streams results back to the caller.
//! Agents must add functionality directly to the susi engine via ToolRegistry,
//! not simulate or "fake" susi capabilities by performing logic themselves.

mod hybrid;
mod master;
mod report;

pub use hybrid::SusiHybridAgent;
pub use master::SusiMasterAgent;
pub use report::{SusiMissionReport, SusiSwarmReport};
