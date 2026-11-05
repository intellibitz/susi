use super::{Adapter, AgentManager, RunRecord, RunStatus};
use std::fs::OpenOptions;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use susi_error::{EaiResult as Result, ResultExt as Context};

/// Resolve `{model}` for adapters that need an LM id (e.g. SWE-agent).
fn resolve_model_placeholder() -> String {
    for key in [
        "SWE_AGENT_MODEL",
        "AIDER_MODEL",
        "LLM_MODEL",
        "ANTHROPIC_MODEL",
        "OPENAI_MODEL",
    ] {
        let v = crate::susi_config::env_or_cloud_env(key).unwrap_or_default();
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    // Sensible default matching SWE-agent hello-world docs.
    "claude-sonnet-4-20250514".into()
}

pub(super) struct OwnedChild(Child);
impl Drop for OwnedChild {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // A separate group isolates cancellation from susi and other agents.
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(self.0.id()) {
            // SAFETY: kill takes a process-group ID and a constant signal; no pointers.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl OwnedChild {
    pub(super) fn id(&self) -> u32 {
        self.0.id()
    }
    pub(super) fn take_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.0.stdin.take()
    }
    pub(super) fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.0.stdout.take()
    }
}

/// Spawn in its own process group; dropping the handle kills and reaps it.
pub(super) fn spawn_owned(cmd: &mut Command) -> Result<OwnedChild> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    Ok(OwnedChild(cmd.spawn().context("start agent process")?))
}

#[allow(clippy::wildcard_enum_match_arm)] // only process adapters reach here; every other Adapter kind bails
pub(super) fn execute(manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    let (program, args) = match &run.adapter {
        Adapter::Command { program, args } => (
            program.clone(),
            args.iter()
                .map(|a| {
                    // Only whole argv placeholders are expanded; prompt contents are never reinterpreted.
                    match a.as_str() {
                        "{prompt}" | "{goal}" => run.prompt.clone(),
                        "{workspace}" => run.workspace.to_string_lossy().into_owned(),
                        "{model}" => resolve_model_placeholder(),
                        _ => a.clone(),
                    }
                })
                .collect::<Vec<_>>(),
        ),
        _ => susi_error::eai_bail!("not a process adapter"),
    };
    let program = super::catalog::resolve_program(&program).context("agent executable missing")?;
    let output = OpenOptions::new()
        .create(true)
        .append(true)
        .open(manager.run_dir(&run.id)?.join("stdout.log"))?;
    let error = OpenOptions::new()
        .create(true)
        .append(true)
        .open(manager.run_dir(&run.id)?.join("stderr.log"))?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(&run.workspace)
        .stdin(Stdio::null())
        .stdout(output)
        .stderr(error)
        // Registered cloud.env keys the parent env lacks: agents see the
        // same keys SUSI's own lookups do, without mutating our env.
        .envs(crate::susi_config::cloud_env_overlay())
        .env("SUSI_AGENT_PROMPT", &run.prompt);
    // Armed last so the probes see the agent's final env; refuses launch into
    // a git work tree whose identity the agent could not see.
    let identity = super::git_identity::GitIdentityGuard::arm(&run.workspace, &mut cmd)?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let result = supervise(manager, run, &mut cmd);
    if run.status == RunStatus::Failed && run.error.is_none() {
        // Without this a failed task reports `error: null` and the cause sits
        // unread in stderr.log (e.g. an agent refusing an untrusted workspace).
        run.error = manager
            .run_dir(&run.id)
            .ok()
            .and_then(|d| failure_summary(&d.join("stderr.log")));
    }
    if let Some(guard) = identity {
        if let Some(note) = guard.restore()? {
            run.error = Some(note);
        }
    }
    result
}

/// First and last non-empty stderr lines, credential-redacted and bounded.
fn failure_summary(stderr_log: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let mut head = String::new();
    std::fs::File::open(stderr_log)
        .ok()?
        .take(16 * 1024)
        .read_to_string(&mut head)
        .ok()?;
    let mut lines = head.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next()?;
    let summary = match lines.next_back() {
        Some(last) if last != first => format!("{first} | {last}"),
        _ => first.to_string(),
    };
    let redacted = crate::susi_config::redact_credentials(&summary);
    Some(redacted.chars().take(300).collect())
}

fn supervise(manager: &AgentManager, run: &mut RunRecord, cmd: &mut Command) -> Result<()> {
    let mut child = OwnedChild(cmd.spawn().context("start agent process")?);
    run.pid = Some(child.0.id());
    manager.save(run)?;
    loop {
        if manager.cancel_requested(&run.id)? {
            // Drop kills and reaps the owned process before cancellation is acknowledged.
            drop(child);
            run.status = RunStatus::Cancelled;
            run.pid = None;
            return Ok(());
        }
        if let Some(status) = child.0.try_wait()? {
            run.exit_code = status.code();
            run.status = if status.success() {
                RunStatus::Succeeded
            } else {
                RunStatus::Failed
            };
            run.pid = None;
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external::CatalogKind;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "susi-ext-proc-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn failure_summary_handles_missing_empty_single_multiline_redacts_and_bounds_length() {
        let dir = temp_dir("summary");
        let log = dir.join("stderr.log");
        assert_eq!(failure_summary(&log), None);

        std::fs::write(&log, "   \n\t\n  ").unwrap();
        assert_eq!(failure_summary(&log), None);

        std::fs::write(&log, "\n  single failure line  \n").unwrap();
        assert_eq!(
            failure_summary(&log).as_deref(),
            Some("single failure line")
        );

        std::fs::write(
            &log,
            "first error line\nmiddle noise\nAuthorization: Bearer sk-secret1234567890abcdef\n",
        )
        .unwrap();
        let multi = failure_summary(&log).unwrap();
        assert!(multi.starts_with("first error line | "), "{multi}");
        assert!(!multi.contains("sk-secret1234567890abcdef"), "{multi}");

        let long_line = format!("START-{}-END", "x".repeat(400));
        std::fs::write(&log, &long_line).unwrap();
        let bounded = failure_summary(&log).unwrap();
        assert_eq!(bounded.chars().count(), 300);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn execute_rejects_non_command_adapters() {
        let dir = temp_dir("exec");
        let manager =
            AgentManager::with_config(&dir, CatalogKind::Execution, dir.join("cfg")).unwrap();

        for adapter in [
            Adapter::A2a {
                url: "http://127.0.0.1:18080/a2a".into(),
                token_env: None,
            },
            Adapter::Acp {
                program: "acp-agent".into(),
                args: vec![],
            },
        ] {
            let mut run = RunRecord {
                id: "run-non-proc-1".into(),
                agent: "test-agent".into(),
                workspace: dir.clone(),
                prompt: "verify process guard".into(),
                adapter,
                status: RunStatus::Running,
                created_at: 1,
                updated_at: 1,
                pid: None,
                remote_id: None,
                remote_url: None,
                exit_code: None,
                error: None,
            };
            let err = execute(&manager, &mut run).unwrap_err().to_string();
            assert!(
                err.contains("not a process adapter"),
                "unexpected error: {err}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_model_placeholder_returns_non_empty_model_identifier() {
        assert!(!resolve_model_placeholder().trim().is_empty());
    }

    #[test]
    fn failure_summary_collapses_identical_first_and_last_lines_without_pipe_separator() {
        let dir = temp_dir("collapse");
        let log = dir.join("stderr.log");
        std::fs::write(&log, "fatal: bad revision\n\nfatal: bad revision\n").unwrap();
        let summary = failure_summary(&log);
        assert_eq!(summary, Some("fatal: bad revision".into()));
        assert!(!summary.unwrap().contains(" | "));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failure_summary_trims_leading_and_trailing_blank_lines_from_multiline_stderr() {
        let dir = temp_dir("trim-blanks");
        let log = dir.join("stderr.log");
        std::fs::write(&log, "\n\n  alpha error  \n\n  omega error  \n\n").unwrap();
        assert_eq!(
            failure_summary(&log).as_deref(),
            Some("alpha error | omega error")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
