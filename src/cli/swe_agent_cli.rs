//! Deterministic SWE-agent control plane (no daemon required).
use super::execution_agent_cli::{self, ExecutionAgentPlane};
use anyhow::Result;
use std::path::Path;
use susi_agents::external::{swe_agent_doctor, swe_agent_setup, swe_agent_status, SWE_AGENT_ID};

pub type SweAgentCommands = execution_agent_cli::ExecutionAgentCommands;

const PLANE: ExecutionAgentPlane = ExecutionAgentPlane {
    agent_id: SWE_AGENT_ID,
    process_banner: Some("susi-swe-agent"),
    status: swe_agent_status,
    doctor: swe_agent_doctor,
    setup: swe_agent_setup,
};

pub fn execute(action: Option<SweAgentCommands>, workspace: &Path) -> Result<()> {
    execution_agent_cli::execute(&PLANE, action, workspace)
}
