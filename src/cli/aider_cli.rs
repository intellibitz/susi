//! Deterministic Aider control plane (no daemon required).
use super::execution_agent_cli::{self, ExecutionAgentPlane};
use anyhow::Result;
use std::path::Path;
use susi_agents::external::{aider_doctor, aider_setup, aider_status, AIDER_AGENT_ID};

pub type AiderCommands = execution_agent_cli::ExecutionAgentCommands;

const PLANE: ExecutionAgentPlane = ExecutionAgentPlane {
    agent_id: AIDER_AGENT_ID,
    process_banner: Some("susi-aider"),
    status: aider_status,
    doctor: aider_doctor,
    setup: aider_setup,
};

pub fn execute(action: Option<AiderCommands>, workspace: &Path) -> Result<()> {
    execution_agent_cli::execute(&PLANE, action, workspace)
}
