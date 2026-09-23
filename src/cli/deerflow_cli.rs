//! Deterministic DeerFlow control plane (no daemon required).
use super::execution_agent_cli::{self, ExecutionAgentPlane};
use anyhow::Result;
use std::path::Path;
use susi_agents::external::{deerflow_doctor, deerflow_setup, deerflow_status, DEERFLOW_AGENT_ID};

pub type DeerFlowCommands = execution_agent_cli::ExecutionAgentCommands;

const PLANE: ExecutionAgentPlane = ExecutionAgentPlane {
    agent_id: DEERFLOW_AGENT_ID,
    process_banner: Some("susi-deerflow"),
    status: deerflow_status,
    doctor: deerflow_doctor,
    setup: deerflow_setup,
};

pub fn execute(action: Option<DeerFlowCommands>, workspace: &Path) -> Result<()> {
    execution_agent_cli::execute(&PLANE, action, workspace)
}
