//! A2A (Agent2Agent Protocol) implementation for susi-gawd
//!
//! This module provides a complete A2A-compliant agent implementation that
//! enables susi-gawd to communicate with other A2A agents in a federated swarm.

pub mod capabilities;
pub mod executor;
pub mod task_store;

pub use capabilities::GawdCapabilities;
pub use executor::GawdA2AExecutor;
pub use task_store::GawdTaskStore;
