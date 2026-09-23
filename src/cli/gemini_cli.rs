//! Deterministic Gemini CLI control plane (no daemon required).
use super::execution_agent_cli::{self, ExecutionAgentPlane};
use anyhow::Result;
use std::path::Path;
use susi_agents::external::{
    gemini_cli_doctor, gemini_cli_setup, gemini_cli_status, GEMINI_CLI_AGENT_ID,
};

pub type GeminiCliCommands = execution_agent_cli::ExecutionAgentCommands;

const PLANE: ExecutionAgentPlane = ExecutionAgentPlane {
    agent_id: GEMINI_CLI_AGENT_ID,
    process_banner: Some("susi-gemini-cli"),
    status: gemini_cli_status,
    doctor: gemini_cli_doctor,
    setup: gemini_cli_setup,
};

pub fn execute(action: Option<GeminiCliCommands>, workspace: &Path) -> Result<()> {
    execution_agent_cli::execute(&PLANE, action, workspace)
}
