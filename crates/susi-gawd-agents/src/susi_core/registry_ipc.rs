//! IPC backend for `registry` — capability dispatch across vendored copies.
//!
//! [`IpcCapabilityRegistry`] keeps the same mental model as
//! [`crate::susi_core::registry::CapabilityRegistry`] but each entry lives in exactly one
//! copy: the registering copy owns the trait object, serves it through a
//! `capability.tool.<name>` / `capability.provider.<name>` topic on its
//! [`IpcPlaneBus`], and writes a small metadata file into the shared
//! `<cache>/bus/<pid>/caps/` rendezvous so other copies can discover it.
//! Lookups in other copies return a `RemoteTool` / `RemoteProvider` proxy that
//! forwards calls over the bus.
//!
//! Enforcement stays at the execution boundary: the owner-side dispatch
//! handler runs [`MacPolicy::authorize_tool`] and [`EvidenceSession`] capture
//! around the real tool — a remote caller cannot bypass MAC by talking to the
//! endpoint directly.
//!
//! Registrations are process-scoped exactly like the in-proc registry —
//! daemon, CLI, and each test get their own `<pid>` rendezvous.

use crate::susi_core::capture::EvidenceSession;
use crate::susi_core::mac_policy::MacPolicy;
use crate::susi_core::plane_bus::PlaneHandler;
use crate::susi_core::plane_bus_ipc::{enc, IpcPlaneBus};
use crate::susi_core::provider::{BoxFuture, Provider};
use crate::susi_core::registry::{AgentCapability, Tool};
use crate::susi_error::{EaiError, EaiResult};
use dashmap::DashMap;
use serde_json::{json, Value};
use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

fn tool_topic(name: &str) -> String {
    format!("capability.tool.{name}")
}

fn provider_topic(name: &str) -> String {
    format!("capability.provider.{name}")
}

/// Write capability metadata atomically into the rendezvous `caps/` tree.
fn write_cap(dir: &Path, name: &str, meta: &Value) {
    let Ok(json) = serde_json::to_vec(meta) else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let path = dir.join(enc(name));
    let tmp = dir.join(format!(".{}.{}.tmp", enc(name), std::process::id()));
    if std::fs::write(&tmp, &json).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn read_cap(dir: &Path, name: &str) -> Option<Value> {
    std::fs::read_to_string(dir.join(enc(name)))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

fn scan_caps(dir: &Path) -> Vec<Value> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || name.ends_with(".tmp") {
                return None;
            }
            std::fs::read_to_string(e.path())
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
        })
        .collect()
}

/// Enforcement boundary applied once at registration: MAC authorize +
/// evidence capture wrap the stored tool, so local `get_tool` lookups and
/// remote bus dispatches get identical observed semantics — and a remote
/// caller cannot bypass authorization by hitting the endpoint directly.
struct EnforcedTool {
    inner: Arc<dyn Tool>,
}

impl Tool for EnforcedTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn execute(&self, args: &Value, workspace: &Path) -> EaiResult<String> {
        MacPolicy::global().authorize_tool(self.name(), args, workspace, None)?;
        EvidenceSession::capture_call(self.name(), args, workspace, || {
            self.inner.execute(args, workspace)
        })
    }
}

/// Owner-side dispatch: forwards to the (already enforcement-wrapped) tool.
struct ToolDispatch {
    tool: Arc<dyn Tool>,
}

impl PlaneHandler for ToolDispatch {
    fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
        let args = payload.get("args").cloned().unwrap_or(Value::Null);
        let ws = payload
            .get("workspace")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string();
        self.tool
            .execute(&args, Path::new(&ws))
            .map(|out| json!({ "output": out }))
            .map_err(|e| e.to_string())
    }
}

/// Proxy implementing the local `Tool` trait; `execute` is forwarded over the
/// bus to the copy that owns the capability.
struct RemoteTool {
    bus: Arc<IpcPlaneBus>,
    name: String,
    description: String,
}

impl Tool for RemoteTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn execute(&self, args: &Value, workspace: &Path) -> EaiResult<String> {
        let resp = self
            .bus
            .request(
                &tool_topic(&self.name),
                json!({
                    "args": args,
                    "workspace": workspace.to_string_lossy(),
                }),
            )
            .map_err(|e| {
                EaiError::network(format!("capability tool `{}` unreachable: {e}", self.name))
            })?;
        resp.get("output")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| {
                EaiError::process(format!(
                    "capability tool `{}`: malformed dispatch response",
                    self.name
                ))
            })
    }
}

/// Owner-side provider dispatch. Provider methods are async; the owner copy
/// blocks them on a lazily-built current-thread tokio runtime held by the
/// registry.
struct ProviderDispatch {
    provider: Arc<dyn Provider>,
    rt: Arc<OnceLock<Option<tokio::runtime::Runtime>>>,
}

impl ProviderDispatch {
    fn block_on<F: std::future::Future>(&self, fut: F) -> Result<F::Output, String> {
        match self.rt.get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()
        }) {
            Some(rt) => Ok(rt.block_on(fut)),
            None => Err("provider dispatch runtime unavailable".into()),
        }
    }
}

impl PlaneHandler for ProviderDispatch {
    fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
        let op = payload.get("op").and_then(|v| v.as_str()).unwrap_or("");
        match op {
            "generate" => {
                let prompt = payload
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.block_on(self.provider.generate(&prompt))?
                    .map(|out| json!({ "output": out }))
                    .map_err(|e| e.to_string())
            }
            "embed" => {
                let text = payload
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.block_on(self.provider.embed(&text))?
                    .map(|v| json!({ "embedding": v }))
                    .map_err(|e| e.to_string())
            }
            "health" => self
                .block_on(self.provider.is_healthy())?
                .map(|h| json!({ "healthy": h }))
                .map_err(|e| e.to_string()),
            other => Err(format!("unknown provider op `{other}`")),
        }
    }
}

/// Proxy implementing the local `Provider` trait; each op is a bus request.
/// The blocking dispatch runs inside the returned future — callers awaiting
/// it on a shared executor should prefer `spawn_blocking` at the call site.
struct RemoteProvider {
    bus: Arc<IpcPlaneBus>,
    name: String,
}

impl Provider for RemoteProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
        let bus = Arc::clone(&self.bus);
        let name = self.name.clone();
        Box::pin(async move {
            let resp = bus
                .request(&provider_topic(&name), json!({ "op": "health" }))
                .map_err(|e| EaiError::network(format!("provider `{name}` unreachable: {e}")))?;
            Ok(resp
                .get("healthy")
                .and_then(|v| v.as_bool())
                .unwrap_or(false))
        })
    }

    fn generate(&self, prompt: &str) -> BoxFuture<'_, EaiResult<String>> {
        let bus = Arc::clone(&self.bus);
        let name = self.name.clone();
        let prompt = prompt.to_string();
        Box::pin(async move {
            let resp = bus
                .request(
                    &provider_topic(&name),
                    json!({ "op": "generate", "prompt": prompt }),
                )
                .map_err(|e| EaiError::network(format!("provider `{name}` unreachable: {e}")))?;
            resp.get("output")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| EaiError::process(format!("provider `{name}`: malformed response")))
        })
    }

    fn embed(&self, text: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
        let bus = Arc::clone(&self.bus);
        let name = self.name.clone();
        let text = text.to_string();
        Box::pin(async move {
            let resp = bus
                .request(
                    &provider_topic(&name),
                    json!({ "op": "embed", "text": text }),
                )
                .map_err(|e| EaiError::network(format!("provider `{name}` unreachable: {e}")))?;
            serde_json::from_value(resp.get("embedding").cloned().unwrap_or(Value::Null))
                .map_err(|e| EaiError::process(format!("provider `{name}`: bad embedding: {e}")))
        })
    }

    /// Downcasting only reaches `RemoteProvider` itself — concrete provider
    /// types cannot cross the IPC boundary.
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Capability registry facade that interoperates across independent vendored
/// copies sharing one `<cache>/bus/<pid>` rendezvous. Registration is local
/// plus a bus topic + metadata file; lookup falls back to a remote proxy.
pub struct IpcCapabilityRegistry {
    bus: Arc<IpcPlaneBus>,
    tools: DashMap<String, Arc<dyn Tool>>,
    providers: DashMap<String, Arc<dyn Provider>>,
    agents: DashMap<String, AgentCapability>,
    provider_rt: Arc<OnceLock<Option<tokio::runtime::Runtime>>>,
}

impl IpcCapabilityRegistry {
    /// Bind a registry facade to an IPC bus. Both copies must be built with
    /// the same rendezvous dir to see each other (use
    /// [`IpcPlaneBus::with_rendezvous`]).
    pub fn new(bus: Arc<IpcPlaneBus>) -> Self {
        Self {
            bus,
            tools: DashMap::new(),
            providers: DashMap::new(),
            agents: DashMap::new(),
            provider_rt: Arc::new(OnceLock::new()),
        }
    }

    fn caps_dir(&self, kind: &str) -> PathBuf {
        self.bus.rendezvous().join("caps").join(kind)
    }

    /// Remove this copy's capability file only when it records this copy's
    /// endpoint — a same-named registration from another copy is left alone.
    fn remove_own_cap(&self, kind: &str, name: &str) {
        let file = self.caps_dir(kind).join(enc(name));
        let ours = self.bus.endpoint().map(|e| e.to_string());
        if let Some(meta) = read_cap(&self.caps_dir(kind), name) {
            let owner = meta.get("endpoint").and_then(|v| v.as_str());
            if owner.is_some() && owner.map(str::to_string) != ours {
                return;
            }
        }
        let _ = std::fs::remove_file(file);
    }

    fn endpoint_tag(&self) -> String {
        self.bus
            .endpoint()
            .map(|e| e.to_string())
            .unwrap_or_default()
    }

    // ---- tools -----------------------------------------------------------

    /// Register a tool: wrapped once with the MAC + capture boundary, kept in
    /// the local map for the fast path, served to other copies through
    /// `capability.tool.<name>` on the bus.
    pub fn register_tool<T: Tool>(&self, tool: T) {
        let name = tool.name().to_string();
        let description = tool.description().to_string();
        let tool: Arc<dyn Tool> = Arc::new(EnforcedTool {
            inner: Arc::new(tool),
        });
        self.tools.insert(name.clone(), Arc::clone(&tool));
        self.bus
            .register(&tool_topic(&name), Arc::new(ToolDispatch { tool }));
        write_cap(
            &self.caps_dir("tool"),
            &name,
            &json!({
                "name": name,
                "description": description,
                "endpoint": self.endpoint_tag(),
            }),
        );
    }

    /// Local copy first; otherwise a remote proxy when another copy serves
    /// the capability (metadata file or wired topic).
    pub fn get_tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if let Some(t) = self.tools.get(name) {
            return Some(Arc::clone(t.value()));
        }
        let meta = read_cap(&self.caps_dir("tool"), name);
        if meta.is_none() && !self.bus.is_wired(&tool_topic(name)) {
            return None;
        }
        let description = meta
            .as_ref()
            .and_then(|m| m.get("description"))
            .and_then(|v| v.as_str())
            .unwrap_or(name)
            .to_string();
        Some(Arc::new(RemoteTool {
            bus: Arc::clone(&self.bus),
            name: name.to_string(),
            description,
        }))
    }

    pub fn unregister_tool(&self, name: &str) -> bool {
        let removed = self.tools.remove(name).is_some();
        self.bus.unregister(&tool_topic(name));
        self.remove_own_cap("tool", name);
        removed
    }

    /// All visible tool names — locally registered plus remote capabilities.
    pub fn list_tools(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tools.iter().map(|e| e.key().clone()).collect();
        for meta in scan_caps(&self.caps_dir("tool")) {
            if let Some(n) = meta.get("name").and_then(|v| v.as_str()) {
                if !names.iter().any(|x| x == n) {
                    names.push(n.to_string());
                }
            }
        }
        names
    }

    // ---- providers ---------------------------------------------------------

    pub fn register_provider(&self, provider: Arc<dyn Provider>) {
        let name = provider.name().to_string();
        self.providers.insert(name.clone(), Arc::clone(&provider));
        self.bus.register(
            &provider_topic(&name),
            Arc::new(ProviderDispatch {
                provider,
                rt: Arc::clone(&self.provider_rt),
            }),
        );
        write_cap(
            &self.caps_dir("provider"),
            &name,
            &json!({ "name": name, "endpoint": self.endpoint_tag() }),
        );
    }

    pub fn get_provider(&self, name: &str) -> Option<Arc<dyn Provider>> {
        if let Some(p) = self.providers.get(name) {
            return Some(Arc::clone(p.value()));
        }
        if read_cap(&self.caps_dir("provider"), name).is_none()
            && !self.bus.is_wired(&provider_topic(name))
        {
            return None;
        }
        Some(Arc::new(RemoteProvider {
            bus: Arc::clone(&self.bus),
            name: name.to_string(),
        }))
    }

    pub fn unregister_provider(&self, name: &str) -> bool {
        let removed = self.providers.remove(name).is_some();
        self.bus.unregister(&provider_topic(name));
        self.remove_own_cap("provider", name);
        removed
    }

    pub fn list_providers(&self) -> Vec<String> {
        let mut names: Vec<String> = self.providers.iter().map(|e| e.key().clone()).collect();
        for meta in scan_caps(&self.caps_dir("provider")) {
            if let Some(n) = meta.get("name").and_then(|v| v.as_str()) {
                if !names.iter().any(|x| x == n) {
                    names.push(n.to_string());
                }
            }
        }
        names
    }

    // ---- agent metadata (pure data — no dispatch) ----------------------------

    pub fn register_agent_capability(&self, cap: AgentCapability) {
        let name = cap.name.clone();
        self.agents.insert(name.clone(), cap.clone());
        write_cap(
            &self.caps_dir("agent"),
            &name,
            &json!({
                "name": cap.name,
                "description": cap.description,
                "is_core": cap.is_core,
            }),
        );
    }

    pub fn get_agent_capability(&self, name: &str) -> Option<AgentCapability> {
        if let Some(c) = self.agents.get(name) {
            return Some(c.value().clone());
        }
        let meta = read_cap(&self.caps_dir("agent"), name)?;
        Some(AgentCapability {
            name: meta.get("name")?.as_str()?.to_string(),
            description: meta
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            is_core: meta
                .get("is_core")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
    }

    pub fn list_agents(&self) -> Vec<AgentCapability> {
        let mut out: Vec<AgentCapability> = self.agents.iter().map(|e| e.value().clone()).collect();
        for meta in scan_caps(&self.caps_dir("agent")) {
            let name = meta.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if !out.iter().any(|c| c.name == name) {
                out.push(AgentCapability {
                    name: name.to_string(),
                    description: meta
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    is_core: meta
                        .get("is_core")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                });
            }
        }
        out
    }

    /// Flat capability listing across all kinds.
    pub fn list_all_capabilities(&self) -> Vec<String> {
        let mut caps = self.list_providers();
        caps.extend(self.list_tools());
        caps.extend(self.list_agents().into_iter().map(|a| a.name));
        caps
    }
}

