//! Durable management of external task executors over Susi's standard protocol
//! sockets (`Command`, `A2a`, `Acp`). Provider output is evidence of execution,
//! never proof that the requested code change is correct.
mod catalog;

pub use catalog::{
    catalog, catalog_with_host_dir, definition, resolve_managed, Adapter, AgentDefinition,
    CatalogKind,
};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use susi_error::{eai_bail as bail, EaiResult as Result, ResultExt as Context};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    Waiting,
    Succeeded,
    Failed,
    Cancelled,
    /// Provider stopped without certifying success.
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
    kind: CatalogKind,
    workspace: PathBuf,
    root: PathBuf,
    config: PathBuf,
    shutdown: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    poll_interval: std::time::Duration,
    /// A fixed catalog in place of the installed one ([`Self::with_catalog`]).
    fixed_catalog: Option<Vec<AgentDefinition>>,
}

/// Explicitly release ownership during normal lexical cleanup, including when a
/// concurrent spawn temporarily inherited the open file description before exec.
/// Never clone or transfer the file: all task mutations stay inside this lease.
struct WorkerLease {
    file: File,
}

impl Drop for WorkerLease {
    fn drop(&mut self) {
        // Closing alone can leave a Unix flock held by an inherited descriptor.
        // Failed unlock stays fail-closed: closing this handle may still leave
        // an inherited duplicate holding the lock. Never panic in Drop.
        let _ = self.file.unlock();
    }
}

fn should_probe_readiness(adapter: &Adapter, verbose: bool) -> bool {
    let _ = (adapter, verbose);
    true
}

/// Execution-catalog entries whose task text is a search query or browsing
/// instruction, not a code change: prepending the self-build contract would
/// corrupt their input.
const NON_CODING_EXECUTORS: &[&str] = &["browser-use", "openviking", "deerflow"];

impl AgentManager {
    pub fn new(workspace: &Path) -> Result<Self> {
        Self::for_kind(workspace, CatalogKind::Execution)
    }

    pub fn frameworks(workspace: &Path) -> Result<Self> {
        Self::for_kind(workspace, CatalogKind::Framework)
    }

    pub fn for_kind(workspace: &Path, kind: CatalogKind) -> Result<Self> {
        Self::with_config(
            workspace,
            kind,
            susi_paths::SusiDirs::config_dir().join(kind.config_subdir()),
        )
    }

    pub fn with_config(workspace: &Path, kind: CatalogKind, config: PathBuf) -> Result<Self> {
        let workspace = workspace
            .canonicalize()
            .context("agent workspace does not exist")?;
        if !workspace.is_dir() {
            bail!("agent workspace must be a directory");
        }
        let root = workspace.join(".susi").join(kind.run_subdir());
        Ok(Self {
            kind,
            workspace,
            root,
            config,
            shutdown: None,
            poll_interval: std::time::Duration::from_secs(3),
            fixed_catalog: None,
        })
    }

    /// Serve `entries` as this manager's catalog instead of the installed one
    /// (the active pack plus the host adapter files in this manager's config
    /// dir). For embedders and tests with their own catalog.
    pub fn with_catalog(mut self, entries: Vec<AgentDefinition>) -> Self {
        self.fixed_catalog = Some(entries);
        self
    }

    /// This manager's catalog: the fixed one, else the installed one — the
    /// active extension pack's entries, then host adapter files in this
    /// manager's config dir ([`catalog_with_host_dir`]).
    pub fn entries(&self) -> Result<Vec<AgentDefinition>> {
        match &self.fixed_catalog {
            Some(entries) => Ok(entries.clone()),
            None => catalog_with_host_dir(self.kind, &self.config),
        }
    }

    fn lookup(&self, id: &str) -> Result<AgentDefinition> {
        self.entries()?
            .into_iter()
            .find(|a| a.id == id || a.peer_name == id)
            .with_context(|| {
                format!(
                    "unknown {} {id}; use {} (catalogs come from an installed extension pack)",
                    self.kind.label(),
                    self.kind.list_hint()
                )
            })
    }

    pub fn kind(&self) -> CatalogKind {
        self.kind
    }

    pub fn with_shutdown(mut self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.shutdown = Some(flag);
        self
    }

    pub fn poll_interval(&self) -> std::time::Duration {
        self.poll_interval
    }

    pub fn adapter(&self, id: &str) -> Result<Adapter> {
        let def = self.lookup(id)?;
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
        let def = self.lookup(id)?;
        adapter.validate()?;
        crate::susi_config::create_private_dir(&self.config)?;
        crate::susi_config::atomic_write_json_pretty(
            &self.config.join(format!("{}.json", def.id)),
            adapter,
        )?;
        Ok(())
    }

    pub fn reset(&self, id: &str) -> Result<()> {
        let def = self.lookup(id)?;
        Ok(crate::susi_config::remove_file_if_present(
            &self.config.join(format!("{}.json", def.id)),
        )?)
    }

    pub fn prepare(&self, agent: &str, prompt: &str) -> Result<RunRecord> {
        let def = self.lookup(agent)?;
        let adapter = self.adapter(&def.id)?;
        self.prepare_adapter(&def.id, prompt, adapter)
    }

    pub fn prepare_adapter(
        &self,
        agent: &str,
        prompt: &str,
        adapter: Adapter,
    ) -> Result<RunRecord> {
        self.lookup(agent)?;
        if prompt.trim().is_empty() {
            bail!("task prompt must not be empty");
        }
        adapter.preflight()?;
        let id = unique_id()?;
        crate::susi_config::create_private_dir(&self.root)?;
        fs::create_dir(self.root.join(&id))?;
        let now = now();
        let prompt =
            if self.kind == CatalogKind::Execution && !NON_CODING_EXECUTORS.contains(&agent) {
                // Mandate 48: a coding agent working on SUSI's own tree gets the
                // self-build contract in its task, whether or not it reads AGENTS.md.
                crate::susi_core::self_build::brief_task(&self.workspace, prompt)
            } else {
                prompt.to_string()
            };
        let run = RunRecord {
            id,
            agent: agent.into(),
            workspace: self.workspace.clone(),
            prompt,
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
        let result: Result<()> = if run.adapter.is_cloud() {
            Err(susi_error::EaiError::internal(
                "A2A external agent plug is in susi-commercial-apps",
            ))
        } else if matches!(run.adapter, Adapter::Acp { .. }) {
            Err(susi_error::EaiError::internal(
                "ACP external agent plug is in susi-commercial-apps",
            ))
        } else {
            Err(susi_error::EaiError::internal(
                "external process agent plug is in susi-commercial-apps",
            ))
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

    /// Zero-config readiness scan: adapters that pass local preflight are marked
    /// ready and advertised on the capability registry.
    pub fn auto_prime_ready(&self) -> Result<Vec<serde_json::Value>> {
        let caps = crate::susi_core::registry::CapabilityRegistry::global();
        let ready = self.scan_ready(&mut |cap| caps.register_agent_capability(cap))?;
        let status_path = susi_paths::SusiDirs::config_dir()
            .join(self.kind.config_subdir())
            .join("ready.json");
        if let Some(parent) = status_path.parent() {
            crate::susi_config::create_private_dir(parent)?;
        }
        crate::susi_config::atomic_write_json_pretty(&status_path, &ready)?;
        Ok(ready)
    }

    /// Scan entries in rank order and register each ready capability.
    /// Readiness is each adapter's local preflight only (PATH lookups and
    /// env checks): the scan spawns no process.
    fn scan_ready(
        &self,
        register: &mut dyn FnMut(crate::susi_core::registry::AgentCapability),
    ) -> Result<Vec<serde_json::Value>> {
        self.scan_ready_with_verbosity(register, std::env::var("SUSI_VERBOSE").is_ok())
    }

    fn scan_ready_with_verbosity(
        &self,
        register: &mut dyn FnMut(crate::susi_core::registry::AgentCapability),
        verbose: bool,
    ) -> Result<Vec<serde_json::Value>> {
        let mut ready = Vec::new();
        let mut catalog = self.entries()?;
        catalog.sort_by_key(|a| a.rank);
        let defs: Vec<_> = catalog
            .into_iter()
            .filter_map(|def| self.adapter(&def.id).ok().map(|adapter| (def, adapter)))
            .filter(|(_, adapter)| should_probe_readiness(adapter, verbose))
            .collect();
        self.register_ready(defs, register, &mut ready);
        Ok(ready)
    }

    fn register_ready(
        &self,
        defs: Vec<(AgentDefinition, Adapter)>,
        register: &mut dyn FnMut(crate::susi_core::registry::AgentCapability),
        ready: &mut Vec<serde_json::Value>,
    ) {
        for (def, adapter) in defs {
            match adapter.preflight() {
                Ok(detail) => {
                    register(crate::susi_core::registry::AgentCapability {
                        name: def.peer_name.clone(),
                        description: format!(
                            "Managed {} `{}` (auto-ready)",
                            self.kind.label(),
                            def.id
                        ),
                        is_core: false,
                    });
                    ready.push(serde_json::json!({
                        "id": def.id,
                        "peer_name": def.peer_name,
                        "kind": self.kind.label(),
                        "ready": true,
                        "detail": detail,
                    }));
                }
                Err(e) => {
                    if std::env::var("SUSI_VERBOSE").is_ok() {
                        eprintln!("[AUTO] {} `{}` not ready: {e}", self.kind.label(), def.id);
                    }
                }
            }
        }
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
                bail!("A2A external agent plug is in susi-commercial-apps");
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
        let run = self.read(id)?;
        if !run.adapter.is_cloud() {
            bail!("refresh is only for cloud tasks; local status comes from the worker");
        }
        bail!("A2A external agent plug is in susi-commercial-apps")
    }

    /// Follow up on an existing cloud session without creating a new billable task.
    pub fn send(&self, id: &str, message: &str) -> Result<RunRecord> {
        if message.trim().is_empty() {
            bail!("message must not be empty");
        }
        let _lease = self.acquire(id)?;
        let run = self.read(id)?;
        if !run.adapter.is_cloud() {
            bail!(
                "local sessions use vendor resume flags through configure; send is for cloud tasks"
            );
        }
        bail!("A2A external agent plug is in susi-commercial-apps")
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

    fn acquire(&self, id: &str) -> Result<WorkerLease> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.run_dir(id)?.join("worker.lock"))?;
        file.try_lock()
            .context("task is owned by a running worker")?;
        Ok(WorkerLease { file })
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
        crate::susi_config::atomic_write_json_pretty(&self.run_dir(&run.id)?.join("run.json"), &run)
    }

    fn cancel_requested(&self, id: &str) -> Result<bool> {
        Ok(self
            .shutdown
            .as_ref()
            .is_some_and(|f| f.load(std::sync::atomic::Ordering::Acquire))
            || self.run_dir(id)?.join("cancel").try_exists()?)
    }
}

/// Generic container runtime probe (`docker info` bounded to 10s).
pub fn docker_present() -> bool {
    crate::susi_core::bounded_cmd::output_within(
        std::process::Command::new("docker").arg("info"),
        std::time::Duration::from_secs(10),
    )
    .is_ok_and(|o| o.status.success())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn unique_id() -> Result<String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    Ok(format!("{nanos:016x}{pid:08x}{seq:08x}"))
}
/// Redact credentials on display; raw vendor logs stay in the private run directory.
pub use crate::susi_config::redact_credentials as redact;
fn tracing_failure(id: &str, error: &str) {
    crate::susi_sandbox::manager::SusiAuditLogger::log(
        &susi_paths::SusiDirs::config_dir(),
        crate::susi_sandbox::manager::LogLevel::Info,
        "EXTERNAL_AGENT",
        &format!("{id}: {}", redact(error)),
    );
}

#[cfg(test)]
#[path = "../tests/external.rs"]
mod unit_tests;
