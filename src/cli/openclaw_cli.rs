//! Deterministic OpenClaw control plane (no daemon required).
use super::execution_agent_cli::{self, ExecutionAgentPlane};
use anyhow::Result;
use std::path::Path;
use susi_agents::external::{openclaw_doctor, openclaw_setup, openclaw_status, OPENCLAW_AGENT_ID};

pub type OpenClawCommands = execution_agent_cli::ExecutionAgentCommands;

const PLANE: ExecutionAgentPlane = ExecutionAgentPlane {
    agent_id: OPENCLAW_AGENT_ID,
    process_banner: Some("susi-openclaw"),
    status: openclaw_status,
    doctor: openclaw_doctor,
    setup: openclaw_setup,
};

pub fn execute(action: Option<OpenClawCommands>, workspace: &Path) -> Result<()> {
    execution_agent_cli::execute(&PLANE, action, workspace)
}
