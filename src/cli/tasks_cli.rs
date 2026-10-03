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
        /// Who is adding it (default: $SUSI_AGENT, your worktree branch, or your git user)
        #[arg(long)]
        agent: Option<String>,
    },
    /// Roadmap coverage and mastery: which vectors have tasks, and which of
    /// them claim delivery in their own narrative
    Roadmap {
        /// Only vectors no task is linked to
        #[arg(long)]
        uncovered: bool,
        /// Only vectors that do not claim delivery and have nothing queued
        #[arg(long)]
        unqueued: bool,
    },
    /// Claim a task (atomic; fails if someone else holds a live claim)
    Claim {
        id: String,
        /// Reserve a repo-relative file/directory (repeatable); overlapping claims fail.
        #[arg(long = "scope")]
        scopes: Vec<String>,
        /// Claim with no reservation at all (deliberate: the commit hook and CI
        /// then refuse any code under it)
        #[arg(long)]
        unscoped: bool,
        #[arg(long)]
        agent: Option<String>,
        /// Lease length; an expired claim can be taken over. Defaults to 4h, or
        /// 12h for a size-l task, which will not finish inside a shorter one.
        #[arg(long)]
        hours: Option<u64>,
    },
    /// Renew an owned live task lease, retaining its scopes.
    Renew {
        id: String,
        #[arg(long)]
        agent: Option<String>,
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
        /// Give up an accepted task whose work is not merged yet, recording why.
        /// Without it a close that is not on origin/main is held, not dropped.
        #[arg(long)]
        abandon: Option<String>,
    },
    /// Run the acceptance check and, only if it passes, close the task
    Close {
        id: String,
        #[arg(long)]
        agent: Option<String>,
    },
    /// Reconcile what agents accepted against what actually reached origin/main
    Audit {
        /// Machine-readable report — the per-agent ranking input (Mandate 56)
        #[arg(long)]
        json: bool,
        /// Exit non-zero when any accepted task is still unpublished
        #[arg(long)]
        strict: bool,
    },
    /// Re-run the acceptance of merged tasks against this tree (CI, main only)
    #[command(hide = true)]
    VerifyMerged {
        /// How many unverified merges one run may check
        #[arg(long, default_value_t = 5)]
        limit: usize,
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

/// `scripts/susi-worktree.sh` names every auto worktree branch
/// `<agent>-<yyyymmdd>-<hhmmss>`, so inside one the branch names the agent —
/// no `--agent` flag or `SUSI_AGENT` needed.
fn agent_from_branch_name(branch: &str) -> Option<String> {
    let (rest, time) = branch.rsplit_once('-')?;
    let (slug, date) = rest.rsplit_once('-')?;
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(date, 8) || !digits(time, 6) {
        return None;
    }
    let ok = !slug.is_empty()
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    ok.then(|| slug.to_string())
}

fn agent_from_branch(root: &Path) -> Option<String> {
    std::process::Command::new("git")
        .args(["symbolic-ref", "-q", "--short", "HEAD"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| agent_from_branch_name(String::from_utf8_lossy(&o.stdout).trim()))
}

/// The agent named by the tool this process runs inside, or `None`.
///
/// `susi.agent` is set only in worktrees `workflow start` created, and the
/// branch-name rule needs `<agent>-<yyyymmdd>-<hhmmss>`. A worktree with
/// neither — `codex/worktree-20260930`, `claude/load-worktree-e2a385` — fell
/// through to the clone-wide `user.name`, so the primary checkout, a codex
/// worker and a claude worker all resolved to `INTELLIBITZ`: one
/// `refs/claim-agents/INTELLIBITZ`, one `T-INTELLIBITZ-<n>` namespace, and
/// (before release was bound to its branch) the ability to free each other's
/// live claims. The process tree is where the tool actually is, so ask it.
fn agent_from_tool_process() -> Option<String> {
    susi_gawd::zc_agent_identity::detect_agent_from_chain(&tool_process_chain(&process_chain()))
}

/// Keep host tools from leaking into identities of binaries launched by Cargo.
/// A test process inherits the developer's Codex/Claude shell, but that shell
/// is not the wrapper the test is asking us to identify. Direct wrappers still
/// appear before the Cargo boundary and remain detectable.
fn tool_process_chain(chain: &str) -> String {
    let mut direct = String::new();
    for line in chain.lines() {
        let executable = line
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .rsplit('/')
            .next()
            .unwrap_or_default();
        if matches!(executable, "cargo" | "rustc" | "rustdoc") {
            break;
        }
        if !direct.is_empty() {
            direct.push('\n');
        }
        direct.push_str(line);
    }
    direct
}

/// Ancestor command lines, from this process up a bounded number of levels.
/// Empty where `/proc` does not exist (macOS), which just restores the old
/// fallback rather than inventing an identity.
fn process_chain() -> String {
    const MAX_DEPTH: u32 = 6;
    let mut chain = String::new();
    let mut pid = std::process::id();
    for _ in 0..MAX_DEPTH {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            break;
        };
        // `comm` may contain spaces and parentheses, so take the fields after
        // its closing paren: state, then ppid.
        let Some(after_comm) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
            break;
        };
        let Some(parent) = after_comm.split_whitespace().nth(1) else {
            break;
        };
        let Ok(ppid) = parent.parse::<u32>() else {
            break;
        };
        if ppid == 0 || ppid == pid {
            break;
        }
        if let Ok(cmdline) = std::fs::read(format!("/proc/{ppid}/cmdline")) {
            chain.push_str(&argv_head(&cmdline));
            chain.push('\n');
        }
        pid = ppid;
    }
    chain
}

/// The first two argv fields of a NUL-separated `/proc/<pid>/cmdline`: the
/// program's basename, plus the script for an interpreted CLI (`node …/claude`).
///
/// The program's directory is deliberately dropped: a test binary built under
/// a worktree named `susi-claude1` lives at `…/susi-claude1/target/…/tasks_cli`,
/// and matching `claude` against that full path mislabels the run as CLAUDE.
/// The tool is named by the executable, not by where it was built.
///
/// Later fields are arguments, and a shell's argument is the script it runs —
/// searching those mislabelled a run as `CURSOR` purely because the command
/// text happened to mention `cursor-agent`. The tool is named by what runs,
/// not by what is said.
fn argv_head(cmdline: &[u8]) -> String {
    let text = String::from_utf8_lossy(cmdline);
    let mut fields = text.split('\0').filter(|field| !field.is_empty()).take(2);
    let program = fields.next().unwrap_or("");
    let program = program.rsplit('/').next().unwrap_or(program);
    match fields.next() {
        Some(script) => format!("{program} {script}"),
        None => program.to_string(),
    }
}

/// The agent this command acts as, most explicit signal first: `--agent`,
/// `SUSI_AGENT`, the worktree's `susi.agent`, the `<agent>-<date>-<time>`
/// branch, the tool in the process tree, then the clone-wide `user.name`.
pub(crate) fn who(agent: Option<String>, root: &Path) -> String {
    agent
        .or_else(|| std::env::var("SUSI_AGENT").ok())
        .filter(|a| !a.trim().is_empty())
        .or_else(|| {
            std::process::Command::new("git")
                .args(["config", "--get", "susi.agent"])
                .current_dir(root)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| agent_from_branch(root))
        .or_else(agent_from_tool_process)
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
                        "scopes": claim.map(|c| &c.scopes),
                        "branch": claim.and_then(|c| c.branch.as_ref()),
                    })
                })
                .collect();
            let mut out = serde_json::json!({ "open": open, "claims_note": claims_note });
            if done {
                out["done"] = serde_json::to_value(tasks::list_done(&root))?;
            }
            print_json(&out)?;
        }
        TaskCommands::Roadmap {
            uncovered,
            unqueued,
        } => {
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
                            // Coverage: every linked task is closed.
                            "delivered": count(p, &|c| c.delivered()),
                            // Mastery: the vector's own narrative claims delivery.
                            "mastery_claimed": count(p, &|c| c.mastery_claimed()),
                            "unqueued": count(p, &|c| c.unqueued()),
                        }),
                    )
                })
                .collect();
            let rows: Vec<_> = cov
                .iter()
                .filter(|c| (!uncovered || c.uncovered()) && (!unqueued || c.unqueued()))
                .map(|c| {
                    serde_json::json!({
                        "id": c.vector.id,
                        "priority": c.vector.priority,
                        "vector": c.vector.title,
                        "open": c.open,
                        "closed": c.closed,
                        "uncovered": c.uncovered(),
                        "mastery": if c.mastery_claimed() { "claimed" } else { "unverified" },
                        "unqueued": c.unqueued(),
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
        TaskCommands::Claim {
            id,
            agent,
            hours,
            scopes,
            unscoped,
        } => {
            // Re-adopting the task being claimed is the one case where its own
            // close is legitimately in this branch and not yet on main.
            tasks::ensure_synced_for(&root, Some(&id))?;
            // A claim that reserves no paths cannot be enforced: the commit hook
            // and CI refuse code under it, so the mistake would surface only
            // when the agent tries to commit its work. Refuse it here instead,
            // with the deliberate override.
            if scopes.is_empty() && !unscoped {
                bail!(
                    "{id} would reserve no paths, so nothing about it could be checked: an agent \
                     could rewrite a file another agent holds. Pass --scope <path> for each area \
                     you will change (repeatable), or --unscoped to claim it deliberately \
                     (Mandate 50)."
                );
            }
            // Size-l work is integration, and an agent at work does not renew:
            // an expired lease is taken over, which would hand the same task to
            // a second agent mid-flight.
            let hours = hours.unwrap_or_else(|| {
                tasks::list_open(&root)
                    .iter()
                    .find(|t| t.id == id)
                    .map_or(tasks::DEFAULT_LEASE_HOURS, |t| {
                        tasks::lease_hours_for_size(&t.size)
                    })
            });
            let c = tasks::claim_scoped(
                &root,
                &id,
                &who(agent, &root),
                tasks::ClaimOptions {
                    hours,
                    now: tasks::now_unix(),
                    scopes: &scopes,
                },
            )?;
            println!(
                "{} claimed {} until unix {}",
                c.agent, c.task, c.lease_until_unix
            );
        }
        TaskCommands::Renew { id, agent, hours } => {
            let claim = tasks::renew(&root, &id, &who(agent, &root), hours, tasks::now_unix())?;
            println!("renewed {id} until unix {}", claim.lease_until_unix);
        }
        TaskCommands::Release {
            id,
            agent,
            force,
            abandon,
        } => {
            if let Some(reason) = abandon {
                tasks::abandon(&root, &id, &who(agent, &root), &reason)?;
                println!("abandoned {id} — the reason is recorded on refs/abandoned/{id}");
            } else {
                tasks::release(&root, &id, &who(agent, &root), force)?;
                println!("released {id}");
            }
        }
        TaskCommands::Close { id, agent } => match tasks::close(&root, &id, &who(agent, &root)) {
            Ok(t) => println!(
                "closed {} — publish it (`susi workflow finish {}`): the close receipt is on the \
                 remote and blocks another task until it is on origin/main",
                t.id, t.id
            ),
            Err(e) => bail!("{e}"),
        },
        TaskCommands::Audit { json, strict } => {
            let report = tasks::audit(&root)?;
            if json {
                print_json(&report)?;
            } else {
                println!("queue audit — {} agent(s) on record", report.agents.len());
                for record in &report.owed {
                    println!(
                        "❌ {} accepted by {} (head {}) is not on origin/main",
                        record.task,
                        record.agent,
                        record.head.get(..8).unwrap_or(&record.head)
                    );
                }
                for record in &report.abandoned {
                    println!(
                        "⏭️  {} accepted by {}, abandoned: {}",
                        record.task, record.agent, record.reason
                    );
                }
                for record in &report.forged {
                    println!(
                        "⚠️  {} claims a merge ({}) that origin/main does not contain — not counted",
                        record.task,
                        record.merge.get(..8).unwrap_or(&record.merge)
                    );
                }
                println!(
                    "✅ {} merge(s) attested, {} re-verified on main, {} awaiting verification",
                    report.merged.len(),
                    report.merged.len() - report.unverified.len(),
                    report.unverified.len()
                );
            }
            if strict && !report.clean() {
                bail!(
                    "{} accepted task(s) are not on origin/main: {}",
                    report.owed.len(),
                    report
                        .owed
                        .iter()
                        .map(|r| r.task.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        TaskCommands::VerifyMerged { limit } => {
            let outcomes = tasks::verify_merged(&root, limit)?;
            let mut failed = Vec::new();
            for outcome in &outcomes {
                match outcome.result.as_str() {
                    "passed" => println!("✅ {} re-verified on main", outcome.task),
                    result if result.starts_with("skipped") => {
                        println!("⏭️  {} not re-run: {}", outcome.task, outcome.detail)
                    }
                    result => {
                        eprintln!("❌ {} {result}: {}", outcome.task, outcome.detail);
                        failed.push(outcome.task.clone());
                    }
                }
            }
            if outcomes.is_empty() {
                println!("nothing to verify");
            }
            if !failed.is_empty() {
                bail!(
                    "the acceptance of {} no longer passes on the merged tree: {}",
                    failed.join(", "),
                    "fix forward — a merge that does not satisfy its own task is a defect"
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{agent_from_branch_name, argv_head, tool_process_chain};

    /// Only the program and an interpreter's script are searched. A shell's
    /// script is an argument, and searching it labelled a real run `CURSOR`
    /// because the command text merely mentioned `cursor-agent`.
    #[test]
    fn only_the_program_names_the_tool() {
        assert_eq!(
            argv_head(b"/usr/bin/cursor-agent\0--force\0"),
            "cursor-agent --force"
        );
        assert_eq!(
            argv_head(b"node\0/opt/claude-code/cli.js\0--resume\0"),
            "node /opt/claude-code/cli.js"
        );
        // The false positive that motivated this: the marker is in the script.
        assert_eq!(
            argv_head(b"/bin/bash\0-c\0ls /tmp/cursor-agent\0"),
            "bash -c"
        );
        assert_eq!(argv_head(b""), "");
        assert_eq!(argv_head(b"bash\0"), "bash");
    }

    /// A program's directory must not name the tool: a test binary built under
    /// a worktree `susi-claude1` is `…/susi-claude1/target/…/tasks_cli`, and
    /// matching `claude` against that path mislabelled the run as CLAUDE.
    #[test]
    fn argv_head_uses_the_executable_basename() {
        assert_eq!(
            argv_head(b"/home/me/susi-claude1/target/debug/deps/tasks_cli-abc\0a_test\0"),
            "tasks_cli-abc a_test"
        );
        // A real tool binary still names the tool through its basename.
        assert_eq!(
            argv_head(b"/opt/claude/bin/claude\0-p\0prompt\0"),
            "claude -p"
        );
        // An interpreter's script keeps its full path so `node …/claude` works.
        assert_eq!(
            argv_head(b"node\0/opt/claude-code/cli.js\0"),
            "node /opt/claude-code/cli.js"
        );
    }

    #[test]
    fn cargo_test_harness_does_not_inherit_the_host_tool_identity() {
        let chain = "/bin/bash /tmp/cursor-agent\n/usr/bin/cargo test\n/usr/local/bin/codex exec";
        assert_eq!(tool_process_chain(chain), "/bin/bash /tmp/cursor-agent");
        assert_eq!(
            susi_gawd::zc_agent_identity::detect_agent_from_chain(&tool_process_chain(chain)),
            Some("CURSOR".to_string())
        );
        assert_eq!(
            tool_process_chain("/usr/bin/cargo test\n/usr/local/bin/codex exec"),
            ""
        );
    }

    #[test]
    fn worktree_branch_names_its_agent() {
        assert_eq!(
            agent_from_branch_name("devin-20260930-163229"),
            Some("devin".to_string())
        );
        assert_eq!(
            agent_from_branch_name("codex-agent-20260930-120000"),
            Some("codex-agent".to_string())
        );
    }

    #[test]
    fn plain_branches_do_not_pose_as_agents() {
        for b in [
            "main",
            "feature-x",
            "claude/load-worktree-e2a385",
            "wip/experiment",
            "devin-2026",
            "x-20260930-1632299",
            "Devin-20260930-163229",
            "devin-20260930",
        ] {
            assert_eq!(agent_from_branch_name(b), None, "{b}");
        }
    }
}
