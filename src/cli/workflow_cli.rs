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
    /// Create your own worktree on a fresh branch off origin/main, ready to
    /// work in (hooks installed, primary checkout parked). Prints `cd <path>`.
    Start {
        /// Branch/worktree name (default: <agent>-<timestamp>)
        name: Option<String>,
        /// Who you are (default: $SUSI_AGENT or your git user)
        #[arg(long)]
        agent: Option<String>,
    },
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
    let (json, agent) = match action {
        WorkflowCommands::Check { json, agent } => (json, agent),
        WorkflowCommands::Start { name, agent } => return start(cwd, name, agent),
    };
    let root = crate::cli::tasks_cli::repo_root(cwd);
    let agent = crate::cli::tasks_cli::who(agent, &root)
        .to_ascii_uppercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>();
    // Keep the primary checkout's main current (silent unless it advanced;
    // never switches a branch or touches a dirty tree).
    sync_primary(&root);
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

/// `susi workflow start`: the whole "get me a workspace of my own" step, so an
/// agent never has to assemble it from instructions.
fn start(cwd: &Path, name: Option<String>, agent: Option<String>) -> Result<()> {
    let root = crate::cli::tasks_cli::repo_root(cwd);
    let script = root.join("scripts").join("susi-worktree.sh");
    if !script.is_file() {
        bail!("{} not found — is this a susi checkout?", script.display());
    }
    let who = crate::cli::tasks_cli::who(agent, &root);
    let mut cmd = std::process::Command::new(&script);
    cmd.current_dir(&root).env("SUSI_AGENT", who);
    if let Some(n) = name {
        cmd.arg(n);
    }
    // The script narrates on stderr and prints `cd <path>` last on stdout.
    let out = cmd.stderr(std::process::Stdio::inherit()).output()?;
    print!("{}", String::from_utf8_lossy(&out.stdout));
    if !out.status.success() {
        bail!("worktree setup failed");
    }
    Ok(())
}

/// Best-effort `park-primary.sh --sync-only`; reports only when main advanced.
fn sync_primary(root: &Path) {
    let script = root.join("scripts").join("park-primary.sh");
    if !script.is_file() {
        return;
    }
    if let Ok(out) = std::process::Command::new(&script)
        .arg("--sync-only")
        .current_dir(root)
        .stderr(std::process::Stdio::null())
        .output()
    {
        let said = String::from_utf8_lossy(&out.stdout);
        if !said.trim().is_empty() {
            eprintln!("ℹ️  {}", said.trim());
        }
    }
}
