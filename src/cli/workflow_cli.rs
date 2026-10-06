//! `susi workflow check`: run this first. It tells an agent whether it may
//! start work here (own worktree, current with origin/main, hooks installed,
//! a live claim) and prints the exact command that fixes each failure.
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;
use susi_gawd::admin::workflow::{self, State};
use susi_gawd::worker_identity;

#[derive(Debug, Subcommand)]
pub enum WorkflowCommands {
    /// Fetch and merge origin/main into a clean agent worktree.
    Sync,
    /// Verify, close, push, wait for remote merge, sync, then release ownership.
    Finish { id: String },
    /// Keep the clean primary checkout current with remote main (local process).
    Watch,

    /// Create your own worktree on a fresh branch off origin/main, ready to
    /// work in (hooks installed, primary checkout parked). Prints `cd <path>`.
    Start {
        /// Branch/worktree name (default: <agent>-<timestamp>)
        name: Option<String>,
        /// Who you are (default: $SUSI_AGENT, else the agent tool you run
        /// inside — never your git login)
        #[arg(long)]
        agent: Option<String>,
    },
    /// Give this worktree an identity of its own: <TOOL><WORKTREE-ID> as claim
    /// token and git author, never the shared git login or `PRIMARY`. `check`
    /// and `start` do this for you; run it to fix a borrowed identity by hand.
    Identity,
    /// Check that this checkout follows the susi workflow (Mandates 49-51)
    Check {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
        /// Who you are (default: $SUSI_AGENT, your worktree branch, or your git user)
        #[arg(long)]
        agent: Option<String>,
    },
}

pub fn execute(action: WorkflowCommands, cwd: &Path) -> Result<()> {
    let (json, agent) = match action {
        WorkflowCommands::Sync => return run_loop(cwd, &["sync"]),
        WorkflowCommands::Finish { id } => return run_loop(cwd, &["finish", &id]),
        WorkflowCommands::Watch => return run_loop(cwd, &["watch"]),
        WorkflowCommands::Check { json, agent } => (json, agent),
        WorkflowCommands::Start { name, agent } => return start(cwd, name, agent),
        WorkflowCommands::Identity => return identity(cwd),
    };
    let root = crate::cli::tasks_cli::repo_root(cwd);
    // Hooks + ledger merge driver install themselves on any susi command —
    // a fresh clone never needs setup-dev.sh (Mandate: zero-config).
    workflow::ensure_infrastructure(&root);
    // So does the worker's identity: a worktree that arrived with `PRIMARY`, or
    // with commits authored by the shared git login, is given its own before
    // anything is claimed, minted or committed under it.
    let identity =
        worker_identity::ensure(&root, None, explicit_agent(agent.as_deref()).as_deref());
    let agent = crate::cli::tasks_cli::who(agent, &root)
        .to_ascii_uppercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>();
    // Keep the primary checkout's main current (silent unless it advanced;
    // never switches a branch or touches a dirty tree).
    sync_primary(&root);
    let facts = workflow::gather(&root, &agent);
    let mut checks = workflow::evaluate(&facts);
    // Right after "own worktree": whose worktree it is comes before anything else.
    checks.insert(1.min(checks.len()), worker_identity::check(&identity));
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
                "ready: atomic loop — work the claimed task; every commit ends with `Task: <id>`; then merge origin/main → push → sync before the next claim"
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
    let mut cmd = std::process::Command::new(&script);
    cmd.current_dir(&root);
    // Only a name the operator gave is passed on. `who()` would answer with this
    // checkout's identity — `PRIMARY` from the primary checkout, or the shared
    // git login — and every worker started from it would be named after that
    // instead of after itself; the script names the tool it runs inside.
    if let Some(explicit) = explicit_agent(agent.as_deref()) {
        cmd.env("SUSI_AGENT", explicit);
    } else {
        cmd.env_remove("SUSI_AGENT");
    }
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

/// `--agent` or `SUSI_AGENT`: the operator named the worker.
fn explicit_agent(flag: Option<&str>) -> Option<String> {
    flag.map(str::to_string)
        .or_else(|| std::env::var("SUSI_AGENT").ok())
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
}

/// `susi workflow identity`: make this worktree's identity its own, and say so.
fn identity(cwd: &Path) -> Result<()> {
    use worker_identity::Outcome;
    let root = crate::cli::tasks_cli::repo_root(cwd);
    match worker_identity::ensure(&root, None, explicit_agent(None).as_deref()) {
        Outcome::Own(t) => println!("{t} (already this worktree's own)"),
        Outcome::Assigned { token, was } => println!("{token} (assigned; was: {was})"),
        Outcome::Explicit(t) => {
            println!("{t} (from SUSI_AGENT; unset it to use the worktree's own)")
        }
        Outcome::Primary => bail!(
            "the primary checkout is not a worker's place and carries no identity — \
             scripts/susi-worktree.sh makes your own worktree"
        ),
        Outcome::Blocked { why, fix } => bail!("{why}\n   → {fix}"),
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

fn run_loop(cwd: &Path, args: &[&str]) -> Result<()> {
    let root = crate::cli::tasks_cli::repo_root(cwd);
    let status = std::process::Command::new(root.join("scripts/parallel-workflow.sh"))
        .args(args)
        .env("SUSI_WORKFLOW_BIN", std::env::current_exe()?)
        .current_dir(&root)
        .status()?;
    if !status.success() {
        bail!("workflow operation failed; resolve the reported blocker and retry");
    }
    Ok(())
}
