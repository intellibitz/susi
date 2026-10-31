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
    /// Verify, close, push, publish the close receipt and release ownership;
    /// the branch run and the merge are the machinery's (a red run is
    /// reported at the next `sync`).
    Finish {
        id: String,
        /// Poll the branch run in-line instead: stop on a red run, hand off on
        /// green or at the SUSI_FINISH_WAIT_MAX budget (the old default)
        #[arg(long, conflicts_with = "no_wait")]
        wait: bool,
        /// Hand off at the push (the default; kept for scripts that pass it)
        #[arg(long)]
        no_wait: bool,
    },
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
    /// Reclaim finished agent worktrees that have merged into origin/main
    Prune {
        /// Actually remove the worktrees (default: report only)
        #[arg(long)]
        apply: bool,
        /// Remove finished worktrees of all agents, not just yours
        #[arg(long)]
        all: bool,
    },
    #[command(name = "own-identity")]
    OwnIdentity,
    /// Mandates 49/50 at the git hooks (`.githooks/workflow-guard` execs this):
    /// refuse commits in the primary checkout or on main, and a push that
    /// deletes, rewrites or is behind origin/main. `push` reads the refs git
    /// hands the pre-push hook on stdin. Needs git only: no shell or coreutils.
    Guard {
        /// The hook that is running
        #[arg(value_enum)]
        stage: GuardStage,
    },
    /// Fetch branch tips, task claims and close receipts from the remote in
    /// one round trip: the snapshot the push guard and CI's compliance job
    /// judge from (`scripts/fetch-workflow-refs.sh` is CI's copy of it).
    #[command(name = "fetch-refs")]
    FetchRefs {
        /// The remote to fetch from
        #[arg(default_value = "origin")]
        remote: String,
    },
    /// Give a worktree a cargo target of its own, warm from the clone's
    /// snapshot of origin/main (`scripts/seed-target.sh` execs this): a link
    /// to the old shared target is replaced (the link only), a missing or
    /// empty target is copied from the snapshot and stripped to registry and
    /// git crates, a target that holds a build is left alone. Never fails.
    #[command(name = "seed-target")]
    SeedTarget {
        /// Replace a link now and copy in a detached process (the hooks)
        #[arg(long, conflicts_with = "refresh")]
        background: bool,
        /// Rebuild origin/main in a clone of its own and republish the snapshot
        /// (the primary watcher runs this)
        #[arg(long)]
        refresh: bool,
        /// With --refresh: refresh a snapshot younger than SUSI_SEED_REFRESH_SECS too
        #[arg(long, requires = "refresh")]
        force: bool,
        /// With --refresh: write the refresh's report and build output here
        #[arg(long, requires = "refresh", value_name = "FILE")]
        log: Option<std::path::PathBuf>,
        /// The worktree to seed (default: the one this runs in)
        #[arg(conflicts_with = "refresh")]
        dir: Option<std::path::PathBuf>,
    },
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
        WorkflowCommands::Finish { id, wait, no_wait } => {
            if wait {
                return run_loop(cwd, &["finish", &id, "--wait"]);
            } else if no_wait {
                return run_loop(cwd, &["finish", &id, "--no-wait"]);
            } else {
                return run_loop(cwd, &["finish", &id]);
            }
        }
        WorkflowCommands::Watch => return run_loop(cwd, &["watch"]),
        WorkflowCommands::Check { json, agent } => (json, agent),
        WorkflowCommands::Start { name, agent } => return start(cwd, name, agent),
        WorkflowCommands::Prune { apply, all } => return prune(cwd, apply, all),
        WorkflowCommands::OwnIdentity => return own_identity(cwd),
        WorkflowCommands::Guard { stage } => return guard(cwd, stage),
        WorkflowCommands::FetchRefs { remote } => return fetch_refs(cwd, &remote),
        WorkflowCommands::SeedTarget {
            background,
            refresh,
            force,
            log,
            dir,
        } => {
            use susi_gawd::admin::seed;
            if refresh {
                seed::refresh(force, log.as_deref());
            } else {
                // A relative DIR is the caller's: taken from where it runs.
                let dir = dir.map(|d| if d.is_absolute() { d } else { cwd.join(d) });
                seed::seed_worktree(dir.as_deref().or(Some(cwd)), background);
            }
            return Ok(());
        }
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
    let explicit = explicit_agent(agent.as_deref());
    let res = workflow::start_worktree(&root, name.as_deref(), explicit.as_deref())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if res.reused {
        eprintln!(
            "susi-worktree: {} already exists — reusing it",
            res.dest.display()
        );
    }
    eprintln!("worktree: {} (branch {})", res.dest.display(), res.branch);
    eprintln!("ready: atomic loop — `susi workflow check`, then sync→claim one→work→commit→sync→push→sync");
    println!("cd {}", res.dest.display());
    Ok(())
}

fn prune(cwd: &Path, apply: bool, all: bool) -> Result<()> {
    let root = crate::cli::tasks_cli::repo_root(cwd);
    let me = crate::cli::tasks_cli::who(None, &root)
        .to_ascii_uppercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>();
    let report =
        workflow::prune_worktrees(&root, apply, all, &me).map_err(|e| anyhow::anyhow!("{e}"))?;
    for item in &report.items {
        println!("  {:<16} {}", item.status, item.detail);
    }
    println!(
        "summary: kept={}, reclaimed={}, other agents={}, failed={}",
        report.kept, report.reclaimed, report.others, report.failed
    );
    if !apply {
        println!("(dry-run: pass `--apply` to remove reclaimable worktrees)");
    }
    Ok(())
}

/// `workflow-guard <commit|merge|push>`, the hook's argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum GuardStage {
    /// pre-commit
    Commit,
    /// pre-merge-commit
    Merge,
    /// pre-push (the pushed refs on stdin)
    Push,
}

/// `susi workflow guard`: the hook's verdict, printed as the hook printed it.
/// A refusal exits 1 with the reason on stderr; the hook's own usage error
/// (an unknown stage) is clap's exit 2, which `.githooks/run-susi` reads as
/// "this susi does not have the verb" and moves on to the next candidate.
fn guard(cwd: &Path, stage: GuardStage) -> Result<()> {
    use susi_gawd::admin::guard::{self, Overrides, Refused, Stage};
    let root = crate::cli::tasks_cli::repo_root(cwd);
    let stage = match stage {
        GuardStage::Commit => Stage::Commit,
        GuardStage::Merge => Stage::Merge,
        GuardStage::Push => Stage::Push,
    };
    let mut pushed_refs = String::new();
    if stage == Stage::Push {
        use std::io::Read;
        std::io::stdin().read_to_string(&mut pushed_refs)?;
    }
    match guard::run(&root, stage, &pushed_refs, Overrides::from_env()) {
        Ok(()) => Ok(()),
        Err(Refused(why)) => {
            if !why.is_empty() {
                eprintln!("{why}");
            }
            std::process::exit(1)
        }
    }
}

/// `susi workflow fetch-refs [remote]`.
fn fetch_refs(cwd: &Path, remote: &str) -> Result<()> {
    let root = crate::cli::tasks_cli::repo_root(cwd);
    susi_gawd::admin::guard::fetch_workflow_refs(&root, remote).map_err(|e| anyhow::anyhow!("{e}"))
}

/// `--agent` or `SUSI_AGENT`: the operator named the worker.
fn explicit_agent(flag: Option<&str>) -> Option<String> {
    flag.map(str::to_string)
        .or_else(|| std::env::var("SUSI_AGENT").ok())
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
}

/// `susi workflow own-identity`: make this worktree's identity its own, and say so.
fn own_identity(cwd: &Path) -> Result<()> {
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
    let mut command = workflow_script_command(&script, &["--sync-only"]);
    if let Ok(out) = command
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
    // Windows cannot relink a running executable, and `finish` runs cargo for
    // its gate. Hand the long-running machinery to a copy outside the cargo
    // target dir, then let this process exit so the linker can replace it.
    if hand_off_finish(cwd, args)? {
        return Ok(());
    }
    let root = crate::cli::tasks_cli::repo_root(cwd);
    let script = root.join("scripts/parallel-workflow.sh");
    let mut cmd = workflow_script_command(&script, args);
    let status = cmd
        .env("SUSI_WORKFLOW_BIN", std::env::current_exe()?)
        .current_dir(&root)
        .status()?;
    if !status.success() {
        bail!("workflow operation failed; resolve the reported blocker and retry");
    }
    Ok(())
}

/// Set in the staged copy so it runs the finish in place instead of staging
/// itself again (that would never end).
///
/// The handoff below is Windows-only, so these helpers compile only for a
/// Windows build or a test build (their unit tests run on every platform);
/// leaving them in a Unix production build would be dead code under the
/// workspace's `-D warnings` gate.
#[cfg_attr(not(windows), allow(dead_code))]
const FINISH_STAGED_ENV: &str = "SUSI_WORKFLOW_STAGED";

/// Whether an executable lives in a cargo target directory, which is exactly
/// the tree the finish gate rebuilds.
#[cfg(any(windows, test))]
fn exe_in_cargo_target(exe: &Path) -> bool {
    exe.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("target")
    })
}

/// Decide whether `finish` must be handed to a staged copy: only on Windows
/// (the linker cannot replace a running image there), only when this process is
/// the one inside the cargo target dir, and only once (the staged copy carries
/// [`FINISH_STAGED_ENV`]).
#[cfg(any(windows, test))]
fn plan_finish_handoff(exe: &Path, already_staged: bool, windows: bool) -> bool {
    windows && !already_staged && exe_in_cargo_target(exe)
}

/// Copy `src` into a fresh directory under the system temp dir and return the
/// copy. The directory is unique per invocation, so two finishes cannot fight
/// over one staged binary.
#[cfg(any(windows, test))]
fn stage_executable(src: &Path) -> Result<std::path::PathBuf> {
    static STAGE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = STAGE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("susi-finish-{}-{stamp}-{seq}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let name = src
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("susi.exe"));
    let dest = dir.join(name);
    std::fs::copy(src, &dest)?;
    Ok(dest)
}

/// The Windows handoff: run `finish` from a staged copy and return `true` so
/// the caller exits. The original process *must* exit — cargo can only remove
/// and relink `target\debug\susi.exe` once nothing runs it, and waiting for the
/// copy here would keep that lock.
#[cfg(windows)]
fn hand_off_finish(cwd: &Path, args: &[&str]) -> Result<bool> {
    if args.first() != Some(&"finish") {
        return Ok(false);
    }
    let exe = std::env::current_exe()?;
    let staged = std::env::var_os(FINISH_STAGED_ENV).is_some();
    if !plan_finish_handoff(&exe, staged, true) {
        return Ok(false);
    }
    let staged_exe = stage_executable(&exe)?;
    // Forward this exact command line: `susi workflow finish <id>`.
    //
    // For one release, while branches cut before the seeding still carry the
    // real target guard (their finish gate runs it, with this copy's
    // `tasks close`): the copy outlives this process, so a guard hold above
    // it (`target-guard.sh --run cargo run -- workflow finish`) ends while it
    // runs. It must not claim that hold as its own, and takes the target
    // itself when its gate needs it. Remove once no branch predates the
    // seeding.
    let mut child = std::process::Command::new(&staged_exe)
        .args(std::env::args_os().skip(1))
        .current_dir(cwd)
        .env(FINISH_STAGED_ENV, "1")
        .env_remove("SUSI_TARGET_GUARD_HELD")
        .spawn()?;
    // A short handshake catches an immediately failing copy (no shell, no
    // repo) while adding nothing to the handoff itself.
    std::thread::sleep(std::time::Duration::from_millis(250));
    if let Some(status) = child.try_wait()? {
        bail!(
            "the staged finish copy {} exited immediately ({status})",
            staged_exe.display()
        );
    }
    eprintln!(
        "susi: running this finish from the staged copy {} so the gate can relink {}",
        staged_exe.display(),
        exe.display()
    );
    eprintln!("susi: this process exits now; the finish continues in the staged copy above");
    Ok(true)
}

/// Other platforms can replace a running executable, so the finish runs in
/// place and reports its own exit status.
#[cfg(not(windows))]
fn hand_off_finish(_cwd: &Path, _args: &[&str]) -> Result<bool> {
    Ok(false)
}

#[cfg(not(windows))]
fn workflow_script_command(script: &Path, args: &[&str]) -> std::process::Command {
    let mut cmd = std::process::Command::new(script);
    cmd.args(args);
    cmd
}

/// Windows cannot exec a script by its shebang (os error 193): drive it
/// through Git for Windows' POSIX shell, with its userland on PATH
/// ([`susi_gawd::admin::tasks::posix_shell_command`]). Probing a bare `bash`
/// here used to find WSL's System32 stub, which cannot run a Windows-path
/// script; then, outside Git Bash, the shell found was Git's raw
/// `usr/bin/sh.exe` with no `/usr/bin` on PATH, so `workflow sync` and
/// `finish` stopped at the script's first `dirname` and `flock`.
#[cfg(windows)]
fn workflow_script_command(script: &Path, args: &[&str]) -> std::process::Command {
    let mut cmd = susi_gawd::admin::tasks::posix_shell_command("sh");
    cmd.arg(script).args(args);
    cmd
}

/// The loop's scripts need coreutils: `parallel-workflow.sh` opens with
/// `dirname`, and its flock fallback with `readlink` and `mkdir`.
#[cfg(all(test, windows))]
mod script_shell {
    use super::*;

    #[test]
    fn a_workflow_script_runs_with_the_posix_userland() {
        let dir = std::env::temp_dir().join(format!("susi-workflow-shell-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("probe.sh");
        std::fs::write(&script, "mkdir -p made && dirname \"$1/leaf\" > made/out\n").unwrap();
        let status = workflow_script_command(&script, &["a/b"])
            .current_dir(&dir)
            .stdin(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(dir.join("made").join("out"))
                .unwrap()
                .trim(),
            "a/b"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod finish_exec_guard {
    use super::*;

    fn source_file(tag: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("susi-guard-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("susi.exe");
        std::fs::write(&file, bytes).unwrap();
        file
    }

    #[test]
    fn target_dir_detection_matches_a_cargo_build_tree_only() {
        assert!(exe_in_cargo_target(Path::new("repo/target/debug/susi.exe")));
        assert!(exe_in_cargo_target(Path::new(
            "repo/target/x86_64-pc-windows-msvc/debug/susi.exe"
        )));
        assert!(exe_in_cargo_target(Path::new(
            "C:/work/TARGET/debug/susi.exe"
        )));
        assert!(!exe_in_cargo_target(Path::new(
            "home/me/.susi/bin/susi.exe"
        )));
        assert!(!exe_in_cargo_target(Path::new("usr/local/bin/susi")));
    }

    #[test]
    fn handoff_requires_windows_a_target_dir_and_no_marker() {
        let in_target = Path::new("repo/target/debug/susi.exe");
        let installed = Path::new("home/me/.susi/bin/susi.exe");
        assert!(plan_finish_handoff(in_target, false, true));
        assert!(!plan_finish_handoff(in_target, false, false)); // unix relinks fine
        assert!(!plan_finish_handoff(in_target, true, true)); // already staged
        assert!(!plan_finish_handoff(installed, false, true)); // not the tree cargo builds
    }

    #[test]
    fn staged_copy_is_byte_identical_and_unique() {
        let src = source_file("stage", b"staged executable payload");
        let first = stage_executable(&src).unwrap();
        let second = stage_executable(&src).unwrap();
        assert_ne!(first, second, "each finish gets its own directory");
        assert_eq!(std::fs::read(&first).unwrap(), std::fs::read(&src).unwrap());
        assert!(first.starts_with(std::env::temp_dir()));
        assert_eq!(first.file_name(), src.file_name());
        assert!(
            !exe_in_cargo_target(&first),
            "the staged copy lives outside the cargo target dir"
        );
        for path in [first, second] {
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
        }
        let _ = std::fs::remove_dir_all(src.parent().unwrap());
    }
}
