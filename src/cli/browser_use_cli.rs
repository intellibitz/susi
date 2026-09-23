//! Deterministic Browser Use control plane (no daemon required).
use super::execution_agent_cli::{self, ExecutionAgentPlane};
use anyhow::Result;
use std::path::Path;
use susi_agents::external::{
    browser_use_doctor, browser_use_setup, browser_use_status, BROWSER_USE_AGENT_ID,
};

pub type BrowserUseCommands = execution_agent_cli::ExecutionAgentCommands;

const PLANE: ExecutionAgentPlane = ExecutionAgentPlane {
    agent_id: BROWSER_USE_AGENT_ID,
    process_banner: Some("susi-browser-use"),
    status: browser_use_status,
    doctor: browser_use_doctor,
    setup: browser_use_setup,
};

pub fn execute(action: Option<BrowserUseCommands>, workspace: &Path) -> Result<()> {
    execution_agent_cli::execute(&PLANE, action, workspace)
}
