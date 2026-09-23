//! Deterministic OpenViking control plane (no daemon required).
use super::execution_agent_cli::{self, ExecutionAgentPlane};
use anyhow::Result;
use std::path::Path;
use susi_agents::external::{
    openviking_doctor, openviking_setup, openviking_status, OPENVIKING_AGENT_ID,
};

pub type OpenVikingCommands = execution_agent_cli::ExecutionAgentCommands;

const PLANE: ExecutionAgentPlane = ExecutionAgentPlane {
    agent_id: OPENVIKING_AGENT_ID,
    process_banner: Some("susi-openviking"),
    status: openviking_status,
    doctor: openviking_doctor,
    setup: openviking_setup,
};

pub fn execute(action: Option<OpenVikingCommands>, workspace: &Path) -> Result<()> {
    execution_agent_cli::execute(&PLANE, action, workspace)
}
