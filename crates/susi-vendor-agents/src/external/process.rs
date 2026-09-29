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
        Adapter::Qwen { python } => (
            python.clone(),
            vec![
                "-u".into(),
                "-c".into(),
                super::python_bridge::QWEN_BRIDGE.trim_start().into(),
            ],
        ),
        Adapter::Python { python, .. } => (
            python.clone(),
            vec![
                "-u".into(),
                "-c".into(),
                super::python_bridge::FRAMEWORK_BRIDGE.trim_start().into(),
            ],
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
    // OpenHands headless banners pollute JSONL capture; suppress when we can.
    if matches!(&run.adapter, Adapter::Command { program, .. } if program == "openhands") {
        cmd.env("OPENHANDS_SUPPRESS_BANNER", "1");
        // BYOK: translate the registered provider key into LLM_API_KEY/BASE_URL.
        cmd.envs(super::byok::openhands_env(&super::byok::system_lookup));
    }
    // Gemini CLI: isolate from IDE gateway settings when API key auth is available.
    if matches!(&run.adapter, Adapter::Command { program, .. } if program == "gemini") {
        super::gemini_cli::apply_headless_env(&mut cmd, &run.workspace);
    }
    // OpenClaw: optional one-shot model override from env.
    if matches!(&run.adapter, Adapter::Command { program, .. } if program == "openclaw") {
        if let Ok(model) = std::env::var("OPENCLAW_MODEL") {
            let trimmed = model.trim();
            if !trimmed.is_empty() {
                cmd.arg("--model").arg(trimmed);
            }
        }
        if std::env::var_os("SUSI_PROCESS_BANNER").is_none() {
            cmd.env("SUSI_PROCESS_BANNER", "susi-openclaw");
        }
    }
    // Browser Use: optional model override from env.
    if matches!(&run.adapter, Adapter::Command { program, .. } if program == "browser-use") {
        super::browser_use::apply_model_override(&mut cmd);
    }
    if matches!(&run.adapter, Adapter::Command { program, .. } if program == "ov") {
        super::openviking::ensure_process_banner(&mut cmd);
    }
    if matches!(&run.adapter, Adapter::Command { program, .. } if program == "deerflow") {
        super::deerflow::apply_process_env(&mut cmd);
    }
    if let Adapter::Python { config_env, .. } = &run.adapter {
        let path = std::env::var(config_env).context("framework config env missing")?;
        cmd.env("SUSI_FRAMEWORK_CONFIG", path);
    }
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
