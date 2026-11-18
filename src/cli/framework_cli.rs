//! Deterministic agent-framework control plane (LangGraph, CrewAI, …), no daemon required.
use super::agent_cli::{execute_catalog, AgentCommands};
use crate::anyhow::Result;
use std::path::Path;
pub use susi_cli_defs::framework::FrameworkCommands;
use susi_gawd::external::CatalogKind;

pub fn execute(action: FrameworkCommands, workspace: &Path) -> Result<()> {
    use AgentCommands as A;
    use FrameworkCommands as F;
    // Keep framework clap names/help intact; only the execution representation
    // is shared. The framework catalog controls storage and worker dispatch.
    let action = match action {
        F::List => A::List,
        F::Doctor { engine } => A::Doctor { agent: engine },
        F::Setup { engine } => A::Setup { agent: engine },
        F::Configure { engine, file } => A::Configure {
            agent: engine,
            file,
        },
        F::Reset { engine } => A::Reset { agent: engine },
        F::Run {
            engine,
            wait,
            prompt,
        } => A::Run {
            agent: engine,
            wait,
            prompt,
        },
        F::Tasks => A::Tasks,
        F::Status { task_id, refresh } => A::Status { task_id, refresh },
        F::Logs {
            task_id,
            stderr,
            bytes,
        } => A::Logs {
            task_id,
            stderr,
            bytes,
        },
        F::Cancel { task_id } => A::Cancel { task_id },
        F::Retry { task_id, wait } => A::Retry { task_id, wait },
        F::Worker { task_id } => A::Worker { task_id },
    };
    execute_catalog(action, workspace, CatalogKind::Framework)
}
