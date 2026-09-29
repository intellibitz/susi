//! `susi workflow check`: run this first. It tells an agent whether it may
//! start work here (own worktree, current with origin/main, hooks installed,
//! a live claim) and prints the exact command that fixes each failure.
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;
use susi_gawd::admin::workflow::{self, State};

#[derive(Debug, Subcommand)]
pub enum WorkflowCommands {
    /// Check that this checkout follows the susi workflow (Mandates 49-51)
    Check {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
        /// Who you are (default: $SUSI_AGENT or your git user)
        #[arg(long)]
        agent: Option<String>,
    },
}

pub fn execute(action: WorkflowCommands, cwd: &Path) -> Result<()> {
    let WorkflowCommands::Check { json, agent } = action;
    let root = crate::cli::tasks_cli::repo_root(cwd);
    let agent = crate::cli::tasks_cli::who(agent, &root)
        .to_ascii_uppercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>();
    let facts = workflow::gather(&root, &agent);
    let checks = workflow::evaluate(&facts);
    let ok = workflow::ok(&checks);
    if json {
        print_json(&serde_json::json!({ "ok": ok, "agent": agent, "checks": checks }))?;
    } else {
        println!(
            "susi workflow check — {} (agent {agent})",
            facts.root.display()
        );
        for c in &checks {
            let mark = match c.state {
                State::Pass => "✅",
                State::Warn => "⚠️ ",
                State::Fail => "❌",
            };
            println!("{mark} {:<16} {}", c.name, c.detail);
            if !c.fix.is_empty() {
                println!("   → {}", c.fix);
            }
        }
        println!(
            "{}",
            if ok {
                "ready: work from your claimed task and end every commit with `Task: <id>`"
            } else {
                "not ready: fix the ❌ items above before changing anything"
            }
        );
    }
    if !ok {
        bail!("workflow check failed");
    }
    Ok(())
}
