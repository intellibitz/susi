//! Durable management of external task executors. Provider output is evidence of
//! execution, never proof that the requested code change is correct.
mod catalog;
mod cloud;
mod process;

use anyhow::{bail, Context, Result};
pub use catalog::{catalog, definition, Adapter, AgentDefinition};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    Waiting,
    Succeeded,
    Failed,
    Cancelled,
    /// Provider stopped without certifying success (e.g. Manus "stopped").
    Stopped,
    /// Worker or transport lost; do not resubmit a potentially billable cloud task.
    Unknown,
}
impl RunStatus {
    pub fn active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    pub agent: String,
    pub workspace: PathBuf,
    pub prompt: String,
    pub adapter: Adapter,
    pub status: RunStatus,
    pub created_at: u64,
    pub updated_at: u64,
    pub pid: Option<u32>,
    pub remote_id: Option<String>,
    pub remote_url: Option<String>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct AgentManager {
    workspace: PathBuf,
    root: PathBuf,
    config: PathBuf,
    shutdown: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl AgentManager {
    pub fn new(workspace: &Path) -> Result<Self> {
        Self::with_config(
            workspace,
            susi_paths::SusiDirs::config_dir().join("execution-agents"),
        )
    }

    pub fn with_config(workspace: &Path, config: PathBuf) -> Result<Self> {
        let workspace = workspace
            .canonicalize()
            .context("agent workspace does not exist")?;
        if !workspace.is_dir() {
            bail!("agent workspace must be a directory");
        }
        let root = workspace.join(".susi/execution-agents");
        Ok(Self {
            workspace,
            root,
            config,
            shutdown: None,
        })
    }

    pub fn with_shutdown(mut self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.shutdown = Some(flag);
        self
    }

    pub fn adapter(&self, id: &str) -> Result<Adapter> {
        let def = definition(id)?;
        let path = self.config.join(format!("{}.json", def.id));
        let adapter = match fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).context("invalid agent adapter configuration")?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => def.adapter,
            Err(e) => return Err(e.into()),
        };
        adapter.validate()?;
        Ok(adapter)
    }

    /// Overrides are host configuration, never executable instructions auto-loaded from a repo.
    pub fn configure(&self, id: &str, adapter: &Adapter) -> Result<()> {
        let def = definition(id)?;
        adapter.validate()?;
        private_dir(&self.config)?;
        atomic_json(&self.config.join(format!("{}.json", def.id)), adapter)
    }

    pub fn reset(&self, id: &str) -> Result<()> {
        let def = definition(id)?;
        match fs::remove_file(self.config.join(format!("{}.json", def.id))) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn prepare(&self, agent: &str, prompt: &str) -> Result<RunRecord> {
        let def = definition(agent)?;
        let adapter = self.adapter(&def.id)?;
        self.prepare_adapter(&def.id, prompt, adapter)
    }

    pub fn prepare_adapter(
        &self,
        agent: &str,
        prompt: &str,
        adapter: Adapter,
    ) -> Result<RunRecord> {
        definition(agent)?;
        if prompt.trim().is_empty() {
            bail!("task prompt must not be empty");
        }
        adapter.preflight()?;
        let id = unique_id()?;
        private_dir(&self.root)?;
        fs::create_dir(self.root.join(&id))?;
        let now = now();
        let run = RunRecord {
            id,
            agent: agent.into(),
            workspace: self.workspace.clone(),
            prompt: prompt.into(),
            adapter,
            status: RunStatus::Queued,
            created_at: now,
            updated_at: now,
            pid: None,
            remote_id: None,
            remote_url: None,
            exit_code: None,
            error: None,
        };
        self.save(&run)?;
        Ok(run)
    }

    /// Runs in a CLI worker or a host thread. The OS lock prevents duplicate dispatch.
    pub fn execute(&self, id: &str) -> Result<RunRecord> {
        let _lease = self.acquire(id)?;
        let mut run = self.read(id)?;
        if run.status != RunStatus::Queued {
            bail!("task was already dispatched; use refresh for cloud tasks");
        }
        if self.cancel_requested(id)? {
            run.status = RunStatus::Cancelled;
            self.save(&run)?;
            return Ok(run);
        }
        run.status = RunStatus::Running;
        self.save(&run)?;
        let result = if run.adapter.is_cloud() {
            cloud::execute(self, &mut run)
        } else {
            process::execute(self, &mut run)
        };
        if let Err(e) = result {
            // A cloud create may have succeeded despite a dropped response: never retry blindly.
            run.status = if run.adapter.is_cloud() {
                RunStatus::Unknown
            } else {
                RunStatus::Failed
            };
            run.error = Some(e.to_string());
            run.pid = None;
        }
        self.save(&run)?;
        Ok(run)
    }

    /// Start from long-lived hosts (MCP/swarm). CLI uses a detached worker instead.
    pub fn start(&self, agent: &str, prompt: &str) -> Result<RunRecord> {
        let run = self.prepare(agent, prompt)?;
        let manager = self.clone();
        let id = run.id.clone();
        std::thread::Builder::new()
            .name(format!("agent-{id}"))
            .spawn(move || {
                if let Err(e) = manager.execute(&id) {
                    tracing_failure(&id, &e.to_string());
                }
            })
            .context("start agent worker")?;
        Ok(run)
    }

    pub fn read(&self, id: &str) -> Result<RunRecord> {
        let run: RunRecord =
            serde_json::from_slice(&fs::read(self.run_dir(id)?.join("run.json"))?)?;
        if run.id != id || run.workspace != self.workspace {
            bail!("task identity/workspace mismatch");
        }
        Ok(run)
    }

    pub fn status(&self, id: &str) -> Result<RunRecord> {
        let mut run = self.read(id)?;
        if run.status == RunStatus::Running {
            if let Ok(_lease) = self.acquire(id) {
                // Re-read under the lease before mutating durable state.
                run = self.read(id)?;
                if run.status == RunStatus::Running {
                    run.status = RunStatus::Unknown;
                    run.error = Some("worker no longer owns task; cloud tasks can be refreshed; local outcome is unknown".into());
                    self.save(&run)?;
                }
            }
        }
        Ok(run)
    }

    pub fn list(&self) -> Result<Vec<RunRecord>> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        let mut runs = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let id = entry.file_name().to_string_lossy().into_owned();
                runs.push(self.status(&id)?);
            }
        }
        runs.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(runs)
    }

    pub fn cancel(&self, id: &str) -> Result<RunRecord> {
        let run = self.read(id)?;
        if !run.status.active() && !matches!(run.status, RunStatus::Waiting | RunStatus::Unknown) {
            return Ok(run);
        }
        File::create(self.run_dir(id)?.join("cancel"))?.sync_all()?;
        // If the worker has exited, take ownership and perform remote cancellation directly.
        if let Ok(_lease) = self.acquire(id) {
            let mut run = self.read(id)?;
            if run.adapter.is_cloud() && run.remote_id.is_some() {
                cloud::cancel(&run)?;
                run.status = RunStatus::Cancelled;
            } else if run.status == RunStatus::Queued {
                run.status = RunStatus::Cancelled;
            } else if run.status == RunStatus::Running {
                // Never send a signal to a persisted PID: it may now belong to another process.
                run.status = RunStatus::Unknown;
                run.error = Some("worker lost; cannot safely cancel by stale PID".into());
            }
            self.save(&run)?;
        }
        self.status(id)
    }

    pub fn refresh(&self, id: &str) -> Result<RunRecord> {
        let _lease = self.acquire(id)?;
        let mut run = self.read(id)?;
        if !run.adapter.is_cloud() {
            bail!("refresh is only for cloud tasks; local status comes from the worker");
        }
        cloud::refresh(self, &mut run)?;
        run.error = None;
        self.save(&run)?;
        Ok(run)
    }

    /// Follow up on an existing cloud session without creating a new billable task.
    pub fn send(&self, id: &str, message: &str) -> Result<RunRecord> {
        if message.trim().is_empty() {
            bail!("message must not be empty");
        }
        let _lease = self.acquire(id)?;
        let mut run = self.read(id)?;
        if !run.adapter.is_cloud() {
            bail!(
                "local sessions use vendor resume flags through configure; send is for cloud tasks"
            );
        }
        cloud::send(&run, message)?;
        run.status = RunStatus::Unknown;
        run.error = Some("follow-up submitted; use refresh to observe provider status".into());
        self.save(&run)?;
        Ok(run)
    }

    pub fn logs(&self, id: &str, stderr: bool, max_bytes: u64) -> Result<String> {
        self.read(id)?;
        let path = self
            .run_dir(id)?
            .join(if stderr { "stderr.log" } else { "stdout.log" });
        let mut file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
            Err(e) => return Err(e.into()),
        };
        let length = file.metadata()?.len();
        file.seek(SeekFrom::Start(
            length.saturating_sub(max_bytes.min(1024 * 1024)),
        ))?;
        let mut bytes = Vec::new();
        file.take(1024 * 1024).read_to_end(&mut bytes)?;
        Ok(redact(&String::from_utf8_lossy(&bytes)))
    }

    pub fn mark_launch_failed(&self, id: &str, error: &str) -> Result<()> {
        let _lease = self.acquire(id)?;
        let mut run = self.read(id)?;
        if run.status == RunStatus::Queued {
            run.status = RunStatus::Failed;
            run.error = Some(error.into());
            self.save(&run)?;
        }
        Ok(())
    }

    fn acquire(&self, id: &str) -> Result<File> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.run_dir(id)?.join("worker.lock"))?;
        file.try_lock()
            .context("task is owned by a running worker")?;
        Ok(file)
    }

    fn run_dir(&self, id: &str) -> Result<PathBuf> {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            bail!("invalid task ID");
        }
        Ok(self.root.join(id))
    }

    fn save(&self, run: &RunRecord) -> Result<()> {
        let mut run = run.clone();
        run.updated_at = now();
        atomic_json(&self.run_dir(&run.id)?.join("run.json"), &run)
    }

    fn cancel_requested(&self, id: &str) -> Result<bool> {
        Ok(self
            .shutdown
            .as_ref()
            .is_some_and(|f| f.load(std::sync::atomic::Ordering::Acquire))
            || self.run_dir(id)?.join("cancel").try_exists()?)
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn unique_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("task ID entropy: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", unique_id()?));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

/// Redact known credential values on display; raw vendor logs stay in the private run directory.
pub fn redact(text: &str) -> String {
    let mut result = text.to_owned();
    for (key, value) in std::env::vars() {
        if value.len() >= 8
            && (key.ends_with("_API_KEY") || key.ends_with("_TOKEN") || key.ends_with("_SECRET"))
        {
            result = result.replace(&value, "[REDACTED]");
        }
    }
    susi_core::redact::redact_patterns(
        &["sk-".into(), "ghp_".into(), "github_pat_".into()],
        &result,
    )
}
fn tracing_failure(id: &str, error: &str) {
    susi_sandbox::manager::SusiAuditLogger::log(
        &susi_paths::SusiDirs::config_dir(),
        susi_sandbox::manager::LogLevel::Info,
        "EXTERNAL_AGENT",
        &format!("{id}: {}", redact(error)),
    );
}

#[cfg(test)]
mod tests;
