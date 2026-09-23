//! Deterministic OpenHands control plane (no daemon required).
use super::execution_agent_cli::{self, ExecutionAgentPlane};
use anyhow::Result;
use std::path::Path;
use susi_agents::external::{openhands_doctor, openhands_setup, openhands_status};

pub type OpenHandsCommands = execution_agent_cli::ExecutionAgentCommands;

const PLANE: ExecutionAgentPlane = ExecutionAgentPlane {
    agent_id: "openhands",
    process_banner: None,
    status: openhands_status,
    doctor: openhands_doctor,
    setup: openhands_setup,
};

pub fn execute(action: Option<OpenHandsCommands>, workspace: &Path) -> Result<()> {
    execution_agent_cli::execute(&PLANE, action, workspace)
}
