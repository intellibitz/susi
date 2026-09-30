//! `susi tasks`: the shared task queue every agent and user builds susi from.
//! Tasks are files under `.agents/tasks/`; claims are atomic git refs; a task
//! is closed only by its acceptance check passing. See `susi_gawd::admin::tasks`.
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::{Path, PathBuf};
use susi_gawd::admin::tasks;

#[derive(Debug, Subcommand)]
pub enum TaskCommands {
    /// List open tasks with who holds each claim (default)
    List {
        /// Also show closed tasks
        #[arg(long)]
        done: bool,
    },
    /// Add a task; it needs an acceptance command that decides "done"
    Add {
        title: String,
        #[arg(long, default_value = "")]
        goal: String,
        /// s | m | l
        #[arg(long, default_value = "m")]
        size: String,
        /// Task ids that must close first (repeatable)
        #[arg(long = "dep")]
        deps: Vec<String>,
        /// Acceptance command, split on spaces, e.g. "cargo test -p susi-gemi brain"
        #[arg(long)]
        accept: String,
        /// Roadmap vector this delivers (VC-<n>-<n>, from .agents/roadmap.json)
        #[arg(long)]
        roadmap: Option<String>,
        /// Who is adding it (default: $SUSI_AGENT or your git user)
        #[arg(long)]
        agent: Option<String>,
    },
    /// Roadmap coverage: which vectors have tasks, and how many are closed
    Roadmap {
        /// Only vectors no task is linked to
        #[arg(long)]
        uncovered: bool,
    },
    /// Claim a task (atomic; fails if someone else holds a live claim)
    Claim {
        id: String,
        #[arg(long)]
        agent: Option<String>,
        /// Lease length; an expired claim can be taken over
        #[arg(long, default_value_t = tasks::DEFAULT_LEASE_HOURS)]
        hours: u64,
    },
    /// Give up a claim
    Release {
        id: String,
        #[arg(long)]
        agent: Option<String>,
        /// Release a claim held by someone else
        #[arg(long)]
        force: bool,
    },
    /// Run the acceptance check and, only if it passes, close the task
    Close {
        id: String,
        #[arg(long)]
        agent: Option<String>,
    },
}

pub(crate) fn repo_root(cwd: &Path) -> PathBuf {
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .unwrap_or_else(|| cwd.to_path_buf())
}

pub(crate) fn who(agent: Option<String>, root: &Path) -> String {
    agent
        .or_else(|| std::env::var("SUSI_AGENT").ok())
        .filter(|a| !a.trim().is_empty())
        .or_else(|| {
            std::process::Command::new("git")
                .args(["config", "user.name"])
                .current_dir(root)
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| "USER".to_string())
}

pub fn execute(action: Option<TaskCommands>, cwd: &Path) -> Result<()> {
    let root = repo_root(cwd);
    // Task work needs the repo hooks; install them if this clone lacks them.
    susi_gawd::admin::workflow::ensure_infrastructure(&root);
    match action.unwrap_or(TaskCommands::List { done: false }) {
        TaskCommands::List { done } => {
            // Claims live on the remote; an unreachable remote must not hide the
            // tasks themselves.
            let (claims, claims_note) = match tasks::claims(&root) {
                Ok(c) => (c, None),
                Err(e) => (Vec::new(), Some(format!("claims unavailable: {e}"))),
            };
            let now = tasks::now_unix();
            let open: Vec<_> = tasks::list_open(&root)
                .into_iter()
                .map(|t| {
                    let claim = claims.iter().find(|c| c.task == t.id);
                    serde_json::json!({
                        "id": t.id,
                        "title": t.title,
                        "size": t.size,
                        "deps": t.deps,
                        "accept": t.accept.cmd.join(" "),
                        "roadmap": t.roadmap,
                        "claimed_by": claim.filter(|c| !c.expired(now)).map(|c| c.agent.clone()),
                        "lease_until_unix": claim.map(|c| c.lease_until_unix),
                    })
                })
                .collect();
            let mut out = serde_json::json!({ "open": open, "claims_note": claims_note });
            if done {
                out["done"] = serde_json::to_value(tasks::list_done(&root))?;
            }
            print_json(&out)?;
        }
        TaskCommands::Roadmap { uncovered } => {
            let vectors = tasks::roadmap_vectors(&root)?;
            let cov = tasks::roadmap_coverage(
                &vectors,
                &tasks::list_open(&root),
                &tasks::list_done(&root),
            );
            let count = |p: &str, f: &dyn Fn(&tasks::Coverage) -> bool| {
                cov.iter()
                    .filter(|c| c.vector.priority == p && f(c))
                    .count()
            };
            let by_priority: serde_json::Map<String, serde_json::Value> = ["P0", "P1", "P2"]
                .iter()
                .map(|p| {
                    (
                        (*p).to_string(),
                        serde_json::json!({
                            "vectors": count(p, &|_| true),
                            "uncovered": count(p, &|c| c.uncovered()),
                            "delivered": count(p, &|c| c.delivered()),
                        }),
                    )
                })
                .collect();
            let rows: Vec<_> = cov
                .iter()
                .filter(|c| !uncovered || c.uncovered())
                .map(|c| {
                    serde_json::json!({
                        "id": c.vector.id,
                        "priority": c.vector.priority,
                        "vector": c.vector.title,
                        "open": c.open,
                        "closed": c.closed,
                        "uncovered": c.uncovered(),
                    })
                })
                .collect();
            print_json(&serde_json::json!({ "by_priority": by_priority, "vectors": rows }))?;
        }
        TaskCommands::Add {
            title,
            goal,
            size,
            deps,
            accept,
            roadmap,
            agent,
        } => {
            let cmd: Vec<String> = accept.split_whitespace().map(str::to_string).collect();
            let task = tasks::add(
                &root,
                &who(agent, &root),
                tasks::NewTask {
                    title,
                    goal,
                    size,
                    deps,
                    accept: cmd,
                    roadmap,
                },
            )?;
            println!(
                "added {} — commit .agents/tasks/{}.json to share it",
                task.id, task.id
            );
        }
        TaskCommands::Claim { id, agent, hours } => {
            let c = tasks::claim(&root, &id, &who(agent, &root), hours, tasks::now_unix())?;
            println!(
                "{} claimed {} until unix {}",
                c.agent, c.task, c.lease_until_unix
            );
        }
        TaskCommands::Release { id, agent, force } => {
            tasks::release(&root, &id, &who(agent, &root), force)?;
            println!("released {id}");
        }
        TaskCommands::Close { id, agent } => match tasks::close(&root, &id, &who(agent, &root)) {
            Ok(t) => println!(
                "closed {} — moved to .agents/tasks/done/; commit that change",
                t.id
            ),
            Err(e) => bail!("{e}"),
        },
    }
    Ok(())
}
