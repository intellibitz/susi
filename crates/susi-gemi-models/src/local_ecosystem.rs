//! Inventory of the local AI ecosystem installed on this host: inference
//! engines, desktop runtimes and model stores, plus the GPU/NPU backends
//! they could use. Detection is read-only; only [`start`] launches anything,
//! and only on an explicit request.
//!
//! Third-party names, ports and health paths live here (vendor code); the
//! scan is generic over [`Probe`] so it is tested without touching the host.
use serde::Serialize;
use std::cell::RefCell;
use std::path::{Path, PathBuf};

use crate::runtime_lifecycle::{
    Capabilities as RuntimeCaps, InferOutcome, Runtime, RuntimeBackend,
};
use crate::susi_error::{EaiError, EaiResult};

/// Everything the scan needs from the host.
pub trait Probe: Sync {
    fn which(&self, program: &str) -> Option<PathBuf>;
    fn home(&self) -> Option<PathBuf>;
    fn exists(&self, path: &Path) -> bool;
    /// True when `GET http://127.0.0.1:<port><path>` answers 2xx.
    fn http_ok(&self, port: u16, path: &str) -> bool;
    /// First line of `<program> --version`, when it prints one.
    fn version(&self, program: &Path) -> Option<String>;
    fn is_macos_arm(&self) -> bool;
}

struct Def {
    id: &'static str,
    name: &'static str,
    binaries: &'static [&'static str],
    /// Home-relative directories containing runtime data or a model store.
    dirs: &'static [&'static str],
    port: u16,
    health: &'static str,
    /// Argv that starts the server without further input; `None` when it needs
    /// a model or other choice only the user can make.
    start: Option<&'static [&'static str]>,
    /// OpenAI-compatible `/v1` base once running (empty for stores/non-serving).
    openai_compat: bool,
}

const DEFS: &[Def] = &[
    Def {
        id: "ollama",
        name: "Ollama",
        binaries: &["ollama"],
        dirs: &[".ollama"],
        port: 11434,
        health: "/api/version",
        start: Some(&["ollama", "serve"]),
        openai_compat: true,
    },
    Def {
        id: "llama-cpp",
        name: "llama.cpp",
        binaries: &["llama-server", "llama-cli"],
        dirs: &[],
        port: 8080,
        health: "/health",
        start: None,
        openai_compat: true,
    },
    Def {
        id: "vllm",
        name: "vLLM",
        binaries: &["vllm"],
        dirs: &[],
        port: 8000,
        health: "/v1/models",
        start: None,
        openai_compat: true,
    },
    Def {
        id: "sglang",
        name: "SGLang",
        binaries: &["sglang"],
        dirs: &[],
        port: 30000,
        health: "/v1/models",
        start: None,
        openai_compat: true,
    },
    Def {
        id: "localai",
        name: "LocalAI",
        binaries: &["local-ai"],
        dirs: &[],
        port: 8080,
        health: "/readyz",
        start: Some(&["local-ai"]),
        openai_compat: true,
    },
    Def {
        id: "jan",
        name: "Jan",
        binaries: &["jan"],
        dirs: &[".local/share/Jan", "jan"],
        port: 1337,
        health: "/v1/models",
        start: None,
        openai_compat: true,
    },
    Def {
        id: "gpt4all",
        name: "GPT4All",
        binaries: &[],
        dirs: &[".local/share/nomic.ai/GPT4All"],
        port: 4891,
        health: "/v1/models",
        start: None,
        openai_compat: true,
    },
    Def {
        id: "huggingface-cache",
        name: "Hugging Face cache",
        binaries: &[],
        dirs: &[".cache/huggingface/hub"],
        port: 0,
        health: "",
        start: None,
        openai_compat: false,
    },
];

/// Env override for non-default ports: `SUSI_ENGINE_PORTS=jan=1400,ollama=11435`.
pub const PORTS_ENV: &str = "SUSI_ENGINE_PORTS";

/// Parse `id=port` pairs; malformed pairs, unknown ids and port 0 are dropped.
pub fn parse_port_overrides(spec: &str) -> Vec<(String, u16)> {
    spec.split(',')
        .filter_map(|pair| {
            let (id, port) = pair.split_once('=')?;
            let id = id.trim().to_ascii_lowercase();
            let port: u16 = port.trim().parse().ok().filter(|p| *p != 0)?;
            DEFS.iter()
                .any(|d| d.id == id && d.port != 0)
                .then_some((id, port))
        })
        .collect()
}

fn env_overrides() -> Vec<(String, u16)> {
    susi_config::env_or_cloud_env(PORTS_ENV)
        .map(|v| parse_port_overrides(&v))
        .unwrap_or_default()
}

fn port_of(d: &Def, overrides: &[(String, u16)]) -> u16 {
    overrides
        .iter()
        .find(|(id, _)| id == d.id)
        .map_or(d.port, |(_, p)| *p)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Detected {
    pub id: String,
    pub name: String,
    pub installed: bool,
    pub running: bool,
    pub binary: Option<PathBuf>,
    pub version: Option<String>,
    /// Existing install/model directories found under the home directory.
    pub dirs: Vec<PathBuf>,
    /// OpenAI-compatible base URL when the engine is running and speaks it.
    pub endpoint: Option<String>,
    /// `susi ecosystem start <id>` can launch it without further input.
    pub startable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Accelerator {
    pub backend: &'static str,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Ecosystem {
    pub engines: Vec<Detected>,
    pub accelerators: Vec<Accelerator>,
}

/// Host-managed runtimes (Ollama, vLLM, …) self-manage models
/// and lifecycle: the scan can discover and health-probe them, never stage
/// or cancel work inside them. [`RuntimeBackend`] implementation for the
/// VC-201-042 contract — unsupported verbs answer typed errors, and the
/// single health probe is shared between `discover` and `health` so a scan
/// costs one request per engine.
struct DaemonBackend<'p> {
    def: &'static Def,
    /// Binary or model-store evidence already gathered by the scan.
    installed_evidence: bool,
    port: u16,
    probe: &'p dyn Probe,
    probed: RefCell<Option<Result<(), String>>>,
}

impl<'p> DaemonBackend<'p> {
    fn probe_health(&self) -> Result<(), String> {
        if let Some(result) = self.probed.borrow().clone() {
            return result;
        }
        let result = if self.port != 0 && self.probe.http_ok(self.port, self.def.health) {
            Ok(())
        } else {
            Err(format!(
                "{} health probe {} failed on port {}",
                self.def.name, self.def.health, self.port
            ))
        };
        *self.probed.borrow_mut() = Some(result.clone());
        result
    }
}

impl RuntimeBackend for DaemonBackend<'_> {
    fn discover(&self) -> EaiResult<bool> {
        Ok(self.installed_evidence || self.probe_health().is_ok())
    }
    fn load(&self, _model: &str) -> EaiResult<()> {
        Err(EaiError::config(format!(
            "{} is host-managed; susi does not stage models into it through the ecosystem scan",
            self.def.name
        )))
    }
    fn health(&self) -> EaiResult<Result<(), String>> {
        Ok(self.probe_health())
    }
    fn infer(&self, _prompt: &str) -> EaiResult<InferOutcome> {
        Ok(InferOutcome::Unavailable {
            reason: format!(
                "{} is host-managed; inference goes through its own endpoint, not the scan",
                self.def.name
            ),
        })
    }
    fn cancel(&self) -> EaiResult<()> {
        Err(EaiError::config(format!(
            "{} is host-managed; susi holds no task handle to cancel",
            self.def.name
        )))
    }
    fn unload(&self) -> EaiResult<()> {
        Err(EaiError::config(format!(
            "{} is host-managed; susi did not stage its model and will not unload it",
            self.def.name
        )))
    }
}

/// Drive a detected engine through the lifecycle contract: `discover`
/// records install/running evidence and `health` is the readiness probe —
/// an answering health endpoint, never a binary's presence, is what marks
/// a runtime `running`.
fn scan_engine(
    p: &dyn Probe,
    d: &'static Def,
    port: u16,
    binary: &Option<PathBuf>,
    dirs: &[PathBuf],
) -> bool {
    let backend = DaemonBackend {
        def: d,
        installed_evidence: binary.is_some() || (d.port == 0 && !dirs.is_empty()),
        port,
        probe: p,
        probed: RefCell::new(None),
    };
    let mut rt = Runtime::new(
        d.id,
        RuntimeCaps {
            can_cancel: false,
            can_unload: false,
        },
        &backend,
    );
    if rt.discover().is_err() {
        return false;
    }
    matches!(rt.health(), Ok(Ok(())))
}

pub fn scan(p: &dyn Probe) -> Ecosystem {
    scan_with(p, &env_overrides())
}

fn scan_with(p: &dyn Probe, overrides: &[(String, u16)]) -> Ecosystem {
    let home = p.home();
    let next_idx = std::sync::atomic::AtomicUsize::new(0);
    let results: std::sync::Mutex<Vec<(usize, Detected)>> =
        std::sync::Mutex::new(Vec::with_capacity(DEFS.len()));
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| loop {
                let idx = next_idx.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(d) = DEFS.get(idx) else {
                    break;
                };
                let binary = d.binaries.iter().find_map(|b| p.which(b));
                let dirs: Vec<PathBuf> = home
                    .as_ref()
                    .map(|h| {
                        d.dirs
                            .iter()
                            .map(|rel| h.join(rel))
                            .filter(|path| p.exists(path))
                            .collect()
                    })
                    .unwrap_or_default();
                let port = port_of(d, overrides);
                let running = scan_engine(p, d, port, &binary, &dirs);
                // A leftover model directory is not an executable runtime. Stores
                // have no serving endpoint and are identified by their directory.
                let installed = binary.is_some() || running || (d.port == 0 && !dirs.is_empty());
                let detected = Detected {
                    id: d.id.into(),
                    name: d.name.into(),
                    installed,
                    running,
                    version: binary.as_deref().and_then(|b| p.version(b)),
                    endpoint: (running && d.openai_compat)
                        .then(|| format!("http://127.0.0.1:{port}/v1")),
                    startable: d.start.is_some() && binary.is_some() && !running,
                    binary,
                    dirs,
                };
                results
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((idx, detected));
            });
        }
    });
    let mut pairs = results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    pairs.sort_by_key(|(idx, _)| *idx);
    let engines = pairs.into_iter().map(|(_, det)| det).collect();
    Ecosystem {
        engines,
        accelerators: accelerators(p),
    }
}

/// `(name, OpenAI-compatible base URL)` for every serving engine in the table,
/// at its default (or `SUSI_ENGINE_PORTS`-overridden) port, for the HTTP provider probe to try. Bases shared by
/// two engines (8080) appear once, under the first name.
pub fn default_openai_endpoints() -> Vec<(String, String)> {
    openai_endpoints_with(&env_overrides())
}

fn openai_endpoints_with(overrides: &[(String, u16)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for d in DEFS.iter().filter(|d| d.openai_compat && d.port != 0) {
        let base = format!("http://localhost:{}/v1", port_of(d, overrides));
        if !out.iter().any(|(_, b)| *b == base) {
            out.push((d.name.to_string(), base));
        }
    }
    out
}

/// GPU/NPU compute backends evidenced on this host (driver tools or device nodes).
pub fn accelerators(p: &dyn Probe) -> Vec<Accelerator> {
    let mut out = Vec::new();
    let mut add =
        |backend: &'static str, evidence: String| out.push(Accelerator { backend, evidence });
    if let Some(path) = p.which("nvidia-smi") {
        add("cuda", path.display().to_string());
    }
    if let Some(path) = p.which("rocm-smi") {
        add("rocm", path.display().to_string());
    } else if p.exists(Path::new("/dev/kfd")) {
        add("rocm", "/dev/kfd".into());
    }
    if p.is_macos_arm() {
        add("metal", "Apple silicon".into());
        add("ane", "Apple Neural Engine".into());
    }
    if let Some(path) = p.which("vulkaninfo") {
        add("vulkan", path.display().to_string());
    }
    if let Some(path) = p.which("sycl-ls").or_else(|| p.which("xpu-smi")) {
        add("oneapi", path.display().to_string());
    }
    out
}

/// Launch a startable engine detached. Refuses anything not in the table or
/// not installed; the child is reaped in the background.
pub fn start(id: &str, p: &dyn Probe) -> Result<String, String> {
    let def = DEFS
        .iter()
        .find(|d| d.id == id)
        .ok_or_else(|| format!("unknown engine {id}"))?;
    let argv = def
        .start
        .ok_or_else(|| format!("{} needs a model or options; start it yourself", def.name))?;
    let program = p
        .which(argv[0])
        .ok_or_else(|| format!("{} is not installed ({} not on PATH)", def.name, argv[0]))?;
    if p.http_ok(def.port, def.health) {
        return Ok(format!(
            "{} is already running on port {}",
            def.name, def.port
        ));
    }
    let mut command = std::process::Command::new(program);
    // An unattended service: no console window (the one owner of the flag).
    susi_paths::console::without_window(&mut command);
    let mut child = command
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to start {}: {e}", def.name))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(format!("started {} ({})", def.name, argv.join(" ")))
}

/// The explicit command that starts engine `id`.
pub fn start_command(id: &str) -> String {
    format!("susi ecosystem start {id}")
}

/// Why a request that needs a local engine cannot reach it. Core starts no
/// engine at boot; a request that needs one starts it on demand
/// ([`ensure_running_on_port`]), and when that is not possible the caller
/// gets this error, which names the explicit start command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalEngineError {
    /// No engine serving this port is installed on this host.
    NotInstalled { port: u16, engines: Vec<String> },
    /// Installed, but it needs a model or options only the user can choose.
    NeedsManualStart { id: String, name: String },
    /// Installed and startable, but the launch failed.
    StartFailed {
        id: String,
        name: String,
        reason: String,
    },
    /// Launched, but its health endpoint did not answer in time.
    NotReady { id: String, name: String, port: u16 },
}

impl LocalEngineError {
    /// The engine id this error is about (`None` when nothing is installed).
    pub fn engine_id(&self) -> Option<&str> {
        match self {
            Self::NotInstalled { .. } => None,
            Self::NeedsManualStart { id, .. }
            | Self::StartFailed { id, .. }
            | Self::NotReady { id, .. } => Some(id),
        }
    }
}

impl std::fmt::Display for LocalEngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled { port, engines } => write!(
                f,
                "local engine needed on port {port} is not installed ({}); install one, then run `susi ecosystem start <id>`",
                engines.join(", ")
            ),
            Self::NeedsManualStart { id, name } => write!(
                f,
                "local engine {name} is installed but not running, and it needs a model or options to start; start it yourself (`{}` explains what it needs)",
                start_command(id)
            ),
            Self::StartFailed { id, name, reason } => write!(
                f,
                "local engine {name} is installed but could not be started on demand ({reason}); start it with `{}`",
                start_command(id)
            ),
            Self::NotReady { id, name, port } => write!(
                f,
                "local engine {name} was started on demand but did not answer on port {port} in time; check it with `susi ecosystem status` or start it with `{}`",
                start_command(id)
            ),
        }
    }
}

impl std::error::Error for LocalEngineError {}

/// Make sure the local engine serving `port` is up, starting it when a
/// request needs it. `Ok(false)`: already running, or no known engine
/// serves that port (nothing to do). `Ok(true)`: this call started it and it
/// now answers. Only an engine the table can launch unattended is started;
/// everything else is a [`LocalEngineError`] naming the explicit command.
///
/// One on-demand launch is in flight per process at a time: concurrent
/// callers wait for it and then see the engine up. A launch that failed or
/// never became healthy is remembered for [`ON_DEMAND_FAILURE_COOLDOWN`], so
/// the rest of a failover cascade fails fast with the same typed error
/// instead of spawning another engine and waiting again.
pub fn ensure_running_on_port(
    port: u16,
    p: &dyn Probe,
    wait: std::time::Duration,
) -> Result<bool, LocalEngineError> {
    static LAUNCH_FAILURES: std::sync::Mutex<
        std::collections::BTreeMap<u16, (std::time::Instant, LocalEngineError)>,
    > = std::sync::Mutex::new(std::collections::BTreeMap::new());
    // Held for the whole launch: this is the single-flight.
    let mut failures = LAUNCH_FAILURES.lock().unwrap_or_else(|e| e.into_inner());
    ensure_running_on_port_once(&mut failures, port, p, wait, &|id| start(id, p))
}

/// How long a failed on-demand launch is answered from memory.
pub const ON_DEMAND_FAILURE_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);

fn ensure_running_on_port_once(
    failures: &mut std::collections::BTreeMap<u16, (std::time::Instant, LocalEngineError)>,
    port: u16,
    p: &dyn Probe,
    wait: std::time::Duration,
    launch: &dyn Fn(&str) -> Result<String, String>,
) -> Result<bool, LocalEngineError> {
    let recent = failures
        .get(&port)
        .filter(|(at, _)| at.elapsed() < ON_DEMAND_FAILURE_COOLDOWN)
        .map(|(_, err)| err.clone());
    if let Some(err) = recent {
        // Fail fast unless the engine has come up since.
        return if serving_answers(port, p) {
            failures.remove(&port);
            Ok(false)
        } else {
            Err(err)
        };
    }
    let result = ensure_running_on_port_with(port, p, wait, launch);
    match &result {
        Err(err @ (LocalEngineError::StartFailed { .. } | LocalEngineError::NotReady { .. })) => {
            failures.insert(port, (std::time::Instant::now(), err.clone()));
        }
        Ok(_) | Err(_) => {
            failures.remove(&port);
        }
    }
    result
}

/// Every engine definition serving `port` (after `SUSI_ENGINE_PORTS`).
fn serving_on(port: u16) -> Vec<&'static Def> {
    let overrides = env_overrides();
    DEFS.iter()
        .filter(|d| d.port != 0 && port_of(d, &overrides) == port)
        .collect()
}

/// True when an engine that serves `port` answers its health endpoint.
fn serving_answers(port: u16, p: &dyn Probe) -> bool {
    serving_on(port).iter().any(|d| p.http_ok(port, d.health))
}

/// For a user request no registered provider could serve: when an engine
/// that can be launched unattended is installed but not running, start it
/// ([`ensure_running_on_port`]) so its models can be admitted and the
/// request routed again. `Ok(Some(id))`: engine `id` was down and now
/// answers (this call, or a concurrent request it waited for, started it);
/// `Ok(None)`: no such engine is installed, or an installed one already
/// answers (nothing to start).
pub fn ensure_startable_engine_running(
    p: &dyn Probe,
    wait: std::time::Duration,
) -> Result<Option<String>, LocalEngineError> {
    let overrides = env_overrides();
    let installed: Vec<(&Def, u16)> = DEFS
        .iter()
        .filter(|d| d.port != 0 && d.start.is_some())
        .filter(|d| d.binaries.iter().any(|b| p.which(b).is_some()))
        .map(|d| (d, port_of(d, &overrides)))
        .collect();
    if installed.iter().any(|(d, port)| p.http_ok(*port, d.health)) {
        return Ok(None);
    }
    let Some((def, port)) = installed.first() else {
        return Ok(None);
    };
    ensure_running_on_port(*port, p, wait).map(|_| Some(def.id.to_string()))
}

fn ensure_running_on_port_with(
    port: u16,
    p: &dyn Probe,
    wait: std::time::Duration,
    launch: &dyn Fn(&str) -> Result<String, String>,
) -> Result<bool, LocalEngineError> {
    let serving = serving_on(port);
    if serving.is_empty() || serving.iter().any(|d| p.http_ok(port, d.health)) {
        return Ok(false);
    }
    let installed: Vec<&Def> = serving
        .iter()
        .copied()
        .filter(|d| d.binaries.iter().any(|b| p.which(b).is_some()))
        .collect();
    let Some(def) = installed
        .iter()
        .find(|d| d.start.is_some())
        .or_else(|| installed.first())
    else {
        return Err(LocalEngineError::NotInstalled {
            port,
            engines: serving.iter().map(|d| d.id.to_string()).collect(),
        });
    };
    if def.start.is_none() {
        return Err(LocalEngineError::NeedsManualStart {
            id: def.id.into(),
            name: def.name.into(),
        });
    }
    launch(def.id).map_err(|reason| LocalEngineError::StartFailed {
        id: def.id.into(),
        name: def.name.into(),
        reason,
    })?;
    let deadline = std::time::Instant::now() + wait;
    loop {
        if p.http_ok(port, def.health) {
            return Ok(true);
        }
        if std::time::Instant::now() >= deadline {
            return Err(LocalEngineError::NotReady {
                id: def.id.into(),
                name: def.name.into(),
                port,
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// The host, observed without running anything: [`HostProbe`] minus the
/// `<binary> --version` spawn. Unattended scans (daemon boot, periodic
/// maintenance) use this so core starts no third-party program on its own;
/// an explicit `susi ecosystem scan` uses [`HostProbe`] and records versions.
/// Versions an explicit scan recorded are carried forward
/// ([`Self::with_recorded_versions`]) for binaries whose path is unchanged,
/// so an unattended scan never erases them.
#[derive(Debug, Default, Clone)]
pub struct DetectOnlyProbe {
    recorded: std::collections::BTreeMap<PathBuf, String>,
}

impl DetectOnlyProbe {
    /// Answer `version` from what an earlier explicit scan recorded, keyed
    /// by binary path.
    pub fn with_recorded_versions(recorded: std::collections::BTreeMap<PathBuf, String>) -> Self {
        Self { recorded }
    }
}

impl Probe for DetectOnlyProbe {
    fn which(&self, program: &str) -> Option<PathBuf> {
        HostProbe.which(program)
    }
    fn home(&self) -> Option<PathBuf> {
        HostProbe.home()
    }
    fn exists(&self, path: &Path) -> bool {
        HostProbe.exists(path)
    }
    fn http_ok(&self, port: u16, path: &str) -> bool {
        HostProbe.http_ok(port, path)
    }
    fn version(&self, program: &Path) -> Option<String> {
        self.recorded.get(program).cloned()
    }
    fn is_macos_arm(&self) -> bool {
        HostProbe.is_macos_arm()
    }
}

/// The real host.
pub struct HostProbe;

impl Probe for HostProbe {
    fn which(&self, program: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        find_executable(program, std::env::split_paths(&path), cfg!(windows))
    }
    fn home(&self) -> Option<PathBuf> {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
    }
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
    fn http_ok(&self, port: u16, path: &str) -> bool {
        susi_http_transport::http_call("GET", &format!("http://127.0.0.1:{port}{path}"), &[], 1, 0)
            .is_ok_and(|c| (200..300).contains(&c.status))
    }
    fn version(&self, program: &Path) -> Option<String> {
        let out = susi_core::bounded_cmd::output_within(
            std::process::Command::new(program).arg("--version"),
            std::time::Duration::from_secs(3),
        )
        .ok()?;
        let first_line = |bytes: &[u8]| {
            String::from_utf8_lossy(bytes)
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(str::to_string)
        };
        first_line(&out.stdout).or_else(|| first_line(&out.stderr))
    }
    fn is_macos_arm(&self) -> bool {
        cfg!(all(target_os = "macos", target_arch = "aarch64"))
    }
}

/// Resolve launchable binaries without spawning a shell or accepting a
/// workspace-relative PATH entry. Windows process launch needs the .exe suffix;
/// scripts are deliberately not treated as directly executable runtimes.
fn find_executable(
    program: &str,
    directories: impl Iterator<Item = PathBuf>,
    windows: bool,
) -> Option<PathBuf> {
    directories
        .filter(|d| d.is_absolute())
        .flat_map(|d| {
            let executable = d.join(program);
            let suffixed = (windows && executable.extension().is_none())
                .then(|| executable.with_extension("exe"));
            std::iter::once(executable).chain(suffixed)
        })
        .find(|c| {
            c.is_file() && {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    c.metadata()
                        .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
                }
                #[cfg(not(unix))]
                true
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[derive(Default)]
    struct Fake {
        bins: HashMap<&'static str, PathBuf>,
        paths: HashSet<PathBuf>,
        up: HashSet<(u16, &'static str)>,
        mac: bool,
    }
    impl Probe for Fake {
        fn which(&self, p: &str) -> Option<PathBuf> {
            self.bins.get(p).cloned()
        }
        fn home(&self) -> Option<PathBuf> {
            Some("/home/u".into())
        }
        fn exists(&self, p: &Path) -> bool {
            self.paths.contains(p)
        }
        fn http_ok(&self, port: u16, path: &str) -> bool {
            self.up.iter().any(|(p, h)| *p == port && *h == path)
        }
        fn version(&self, _: &Path) -> Option<String> {
            Some("v1".into())
        }
        fn is_macos_arm(&self) -> bool {
            self.mac
        }
    }

    fn engine<'a>(e: &'a Ecosystem, id: &str) -> &'a Detected {
        e.engines.iter().find(|d| d.id == id).unwrap()
    }

    #[test]
    fn empty_host_reports_nothing_installed() {
        let e = scan(&Fake::default());
        assert!(e
            .engines
            .iter()
            .all(|d| !d.installed && !d.running && !d.startable));
        assert!(e.accelerators.is_empty());
    }

    #[test]
    fn installed_but_stopped_engine_is_startable() {
        let mut f = Fake::default();
        f.bins.insert("ollama", "/usr/bin/ollama".into());
        let e = scan(&f);
        let o = engine(&e, "ollama");
        assert!(o.installed && !o.running && o.startable);
        assert_eq!(o.version.as_deref(), Some("v1"));
        assert!(o.endpoint.is_none());
    }

    #[test]
    fn running_engine_exposes_openai_endpoint_and_is_not_startable() {
        let mut f = Fake::default();
        f.bins.insert("ollama", "/usr/bin/ollama".into());
        f.up.insert((11434, "/api/version"));
        let o = scan(&f)
            .engines
            .into_iter()
            .find(|d| d.id == "ollama")
            .unwrap();
        assert!(o.running && !o.startable);
        assert_eq!(o.endpoint.as_deref(), Some("http://127.0.0.1:11434/v1"));
    }

    fn never_launch(id: &str) -> Result<String, String> {
        panic!("{id} must not be launched")
    }

    #[test]
    fn on_demand_start_skips_running_and_unknown_ports() {
        let mut f = Fake::default();
        f.bins.insert("ollama", "/usr/bin/ollama".into());
        f.up.insert((11434, "/api/version"));
        let wait = std::time::Duration::ZERO;
        assert_eq!(
            ensure_running_on_port_with(11434, &f, wait, &never_launch),
            Ok(false)
        );
        // No known engine serves this port: nothing to start.
        assert_eq!(
            ensure_running_on_port_with(54321, &Fake::default(), wait, &never_launch),
            Ok(false)
        );
    }

    #[test]
    fn on_demand_start_launches_an_installed_startable_engine() {
        let mut f = Fake::default();
        f.bins.insert("ollama", "/usr/bin/ollama".into());
        let launched = std::cell::RefCell::new(Vec::new());
        // The fake engine never comes up: the request gets a typed error
        // that names the explicit start command.
        let err = ensure_running_on_port_with(11434, &f, std::time::Duration::ZERO, &|id| {
            launched.borrow_mut().push(id.to_string());
            Ok("started".into())
        })
        .unwrap_err();
        assert_eq!(launched.into_inner(), ["ollama"]);
        assert_eq!(err.engine_id(), Some("ollama"));
        assert!(
            err.to_string().contains("susi ecosystem start ollama"),
            "{err}"
        );
    }

    #[test]
    fn on_demand_start_failures_are_typed_and_name_the_start_command() {
        let wait = std::time::Duration::ZERO;
        let err =
            ensure_running_on_port_with(11434, &Fake::default(), wait, &never_launch).unwrap_err();
        assert!(matches!(
            err,
            LocalEngineError::NotInstalled { port: 11434, .. }
        ));

        let mut f = Fake::default();
        f.bins.insert("vllm", "/usr/bin/vllm".into());
        let err = ensure_running_on_port_with(8000, &f, wait, &never_launch).unwrap_err();
        assert!(
            matches!(err, LocalEngineError::NeedsManualStart { .. }),
            "{err}"
        );
        assert!(
            err.to_string().contains("susi ecosystem start vllm"),
            "{err}"
        );

        let mut f = Fake::default();
        f.bins.insert("ollama", "/usr/bin/ollama".into());
        let err =
            ensure_running_on_port_with(11434, &f, wait, &|_| Err("denied".into())).unwrap_err();
        assert!(matches!(err, LocalEngineError::StartFailed { .. }), "{err}");
        assert!(
            err.to_string().contains("susi ecosystem start ollama"),
            "{err}"
        );
    }

    #[test]
    fn detect_only_probe_runs_no_version_command() {
        let exe = std::env::current_exe().unwrap();
        assert_eq!(DetectOnlyProbe::default().version(&exe), None);
        // An explicit scan's recorded version is carried forward for the
        // same binary path, and only for it.
        let probe = DetectOnlyProbe::with_recorded_versions(
            [(exe.clone(), "v9".to_string())].into_iter().collect(),
        );
        assert_eq!(probe.version(&exe).as_deref(), Some("v9"));
        assert_eq!(probe.version(Path::new("/elsewhere/ollama")), None);
    }

    #[test]
    fn a_failed_on_demand_launch_is_not_repeated_within_the_cooldown() {
        let mut f = Fake::default();
        f.bins.insert("ollama", "/usr/bin/ollama".into());
        let launches = std::cell::Cell::new(0);
        let launch = |_: &str| {
            launches.set(launches.get() + 1);
            Ok("started".to_string())
        };
        let mut failures = std::collections::BTreeMap::new();
        let wait = std::time::Duration::ZERO;
        let first = ensure_running_on_port_once(&mut failures, 11434, &f, wait, &launch);
        assert!(matches!(first, Err(LocalEngineError::NotReady { .. })));
        // The rest of the cascade fails fast with the same typed error.
        let again = ensure_running_on_port_once(&mut failures, 11434, &f, wait, &launch);
        assert_eq!(again, first);
        assert_eq!(launches.get(), 1, "one launch, not one per provider");
        // Once it answers, the remembered failure is dropped.
        f.up.insert((11434, "/api/version"));
        assert_eq!(
            ensure_running_on_port_once(&mut failures, 11434, &f, wait, &launch),
            Ok(false)
        );
        assert!(failures.is_empty());
    }

    #[test]
    fn a_request_with_no_provider_starts_only_a_stopped_startable_engine() {
        // Nothing installed, or an installed engine already answering:
        // nothing to start.
        let wait = std::time::Duration::ZERO;
        assert_eq!(
            ensure_startable_engine_running(&Fake::default(), wait),
            Ok(None)
        );
        let mut f = Fake::default();
        f.bins.insert("ollama", "/usr/bin/ollama".into());
        f.up.insert((11434, "/api/version"));
        assert_eq!(ensure_startable_engine_running(&f, wait), Ok(None));
    }

    #[test]
    fn shared_port_is_disambiguated_by_health_path() {
        let mut f = Fake::default();
        f.up.insert((8080, "/health"));
        let e = scan(&f);
        assert!(engine(&e, "llama-cpp").running);
        assert!(!engine(&e, "localai").running);
    }

    #[test]
    fn directory_alone_proves_install_and_model_store() {
        let mut f = Fake::default();
        f.paths.insert("/home/u/.cache/huggingface/hub".into());
        let e = scan(&f);
        let h = engine(&e, "huggingface-cache");
        assert!(h.installed && !h.running && h.dirs.len() == 1 && h.endpoint.is_none());
    }

    #[test]
    fn leftover_runtime_data_is_not_a_usable_installation() {
        let mut f = Fake::default();
        f.paths.insert("/home/u/.ollama".into());
        let e = scan(&f);
        let o = engine(&e, "ollama");
        assert!(!o.installed && !o.running && !o.startable);
        assert_eq!(o.dirs.len(), 1, "preserve evidence of existing model data");
    }

    #[test]
    fn windows_executable_suffix_is_discovered_without_shell() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("ollama.exe");
        std::fs::write(&executable, "fixture").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert_eq!(
            find_executable("ollama", std::iter::once(dir.path().to_path_buf()), true),
            Some(executable)
        );
        assert!(find_executable("ollama", std::iter::once(PathBuf::from(".")), true).is_none());
    }

    #[test]
    fn slow_engine_probes_overlap_and_keep_catalog_order() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct SlowProbe {
            active: AtomicUsize,
            peak: AtomicUsize,
        }
        impl Probe for SlowProbe {
            fn which(&self, _: &str) -> Option<PathBuf> {
                None
            }
            fn home(&self) -> Option<PathBuf> {
                None
            }
            fn exists(&self, _: &Path) -> bool {
                false
            }
            fn http_ok(&self, _: u16, _: &str) -> bool {
                let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.peak.fetch_max(active, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(30));
                self.active.fetch_sub(1, Ordering::SeqCst);
                false
            }
            fn version(&self, _: &Path) -> Option<String> {
                None
            }
            fn is_macos_arm(&self) -> bool {
                false
            }
        }
        let probe = SlowProbe {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        };
        let e = scan_with(&probe, &[]);
        assert!(
            probe.peak.load(Ordering::SeqCst) > 1,
            "slow probes must not serialize discovery"
        );
        assert!(probe.peak.load(Ordering::SeqCst) <= 4);
        assert_eq!(
            e.engines.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            DEFS.iter().map(|d| d.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn engines_needing_a_model_are_never_startable() {
        let mut f = Fake::default();
        f.bins
            .insert("llama-server", "/usr/bin/llama-server".into());
        assert!(!engine(&scan(&f), "llama-cpp").startable);
        assert!(start("llama-cpp", &f)
            .unwrap_err()
            .contains("needs a model"));
        assert!(start("nope", &f).is_err());
    }

    #[test]
    fn default_endpoints_are_unique_and_include_newer_engines() {
        let eps = default_openai_endpoints();
        let bases: HashSet<_> = eps.iter().map(|(_, b)| b.clone()).collect();
        assert_eq!(bases.len(), eps.len());
        assert!(eps.contains(&("Jan".to_string(), "http://localhost:1337/v1".to_string())));
        assert!(eps.contains(&(
            "Ollama".to_string(),
            "http://localhost:11434/v1".to_string()
        )));
    }

    #[test]
    fn port_overrides_parse_defensively() {
        assert_eq!(
            parse_port_overrides(
                "jan=1400, Ollama=11435,bogus=1,jan=x,vllm=0,huggingface-cache=9,noequals"
            ),
            vec![("jan".to_string(), 1400), ("ollama".to_string(), 11435)]
        );
    }

    #[test]
    fn overridden_port_drives_detection_and_endpoints() {
        let mut f = Fake::default();
        f.up.insert((1400, "/v1/models"));
        let o = [("jan".to_string(), 1400_u16)];
        let e = scan_with(&f, &o);
        let jan = e.engines.iter().find(|d| d.id == "jan").unwrap();
        assert!(jan.running);
        assert_eq!(jan.endpoint.as_deref(), Some("http://127.0.0.1:1400/v1"));
        assert!(openai_endpoints_with(&o)
            .contains(&("Jan".to_string(), "http://localhost:1400/v1".to_string())));
        // Default port no longer counts once overridden.
        f.up.clear();
        f.up.insert((1337, "/v1/models"));
        assert!(
            !scan_with(&f, &o)
                .engines
                .iter()
                .find(|d| d.id == "jan")
                .unwrap()
                .running
        );
    }

    #[test]
    fn accelerators_are_reported_from_evidence() {
        let mut f = Fake::default();
        f.bins.insert("nvidia-smi", "/usr/bin/nvidia-smi".into());
        f.paths.insert("/dev/kfd".into());
        f.mac = true;
        let backends: Vec<_> = accelerators(&f).iter().map(|a| a.backend).collect();
        assert_eq!(backends, ["cuda", "rocm", "metal", "ane"]);
    }
}
