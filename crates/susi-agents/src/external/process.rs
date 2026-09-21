use super::{Adapter, AgentManager, RunRecord, RunStatus};
use anyhow::{Context, Result};
use std::fs::OpenOptions;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct OwnedChild(Child);
impl Drop for OwnedChild {
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
                        _ => a.clone(),
                    }
                })
                .collect::<Vec<_>>(),
        ),
        Adapter::Qwen { python } => (
            python.clone(),
            vec!["-u".into(), "-c".into(), include_str!("qwen.py").into()],
        ),
        Adapter::Python { python, .. } => (
            python.clone(),
            vec!["-u".into(), "-c".into(), include_str!("runner.py").into()],
        ),
        _ => anyhow::bail!("not a process adapter"),
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
        .env("SUSI_AGENT_PROMPT", &run.prompt);
    if let Adapter::Python { config_env, .. } = &run.adapter {
        let path = std::env::var(config_env).context("framework config env missing")?;
        cmd.env("SUSI_FRAMEWORK_CONFIG", path);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
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
