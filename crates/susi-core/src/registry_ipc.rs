//! IPC backend for `registry` — capability dispatch across cell processes.
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
use crate::susi_core::provider::{block_on, BoxFuture, Provider};
use crate::susi_core::registry::{AgentCapability, Tool};
use crate::susi_error::{EaiError, EaiResult};
use serde_json::{json, Value};
use std::any::Any;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;

fn tool_topic(name: &str) -> String {
    format!("capability.tool.{name}")
}

fn provider_topic(name: &str) -> String {
    format!("capability.provider.{name}")
}

fn is_caps_manifest_name(name: &str) -> bool {
    name == "caps.json" || (name.starts_with("caps.") && name.ends_with(".json"))
}

fn read_dir_cap_manifests(dir: &Path) -> Vec<Value> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            is_caps_manifest_name(&name).then(|| e.path())
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|p| {
            std::fs::read_to_string(p)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
        })
        .collect()
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

/// Owner-side provider dispatch. Provider methods return runtime-agnostic
/// futures; the owner copy drives each one to completion on the bus handler
/// thread ([`crate::susi_core::provider::block_on`]).
struct ProviderDispatch {
    provider: Arc<dyn Provider>,
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
                block_on(self.provider.generate(&prompt))
                    .map(|out| json!({ "output": out }))
                    .map_err(|e| e.to_string())
            }
            "embed" => {
                let text = payload
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                block_on(self.provider.embed(&text))
                    .map(|v| json!({ "embedding": v }))
                    .map_err(|e| e.to_string())
            }
            "health" => block_on(self.provider.is_healthy())
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

struct CapBatchGuard<'a> {
    reg: &'a IpcCapabilityRegistry,
}

impl Drop for CapBatchGuard<'_> {
    fn drop(&mut self) {
        self.reg.end_batch();
    }
}

/// Capability registry facade that interoperates across independent
/// processes sharing one `<cache>/bus/<pid>` rendezvous. Registration is local
/// plus a bus topic + single per-copy manifest file (`caps.json`); lookup falls
/// back to a remote proxy.
#[derive(Clone)]
pub struct IpcCapabilityRegistry {
    bus: Arc<IpcPlaneBus>,
    #[allow(clippy::type_complexity)]
    tools: Arc<RwLock<HashMap<String, Arc<dyn Tool>>>>,
    #[allow(clippy::type_complexity)]
    providers: Arc<RwLock<HashMap<String, Arc<dyn Provider>>>>,
    agents: Arc<RwLock<HashMap<String, AgentCapability>>>,
    batch_depth: Arc<std::sync::atomic::AtomicUsize>,
    dirty_caps: Arc<std::sync::atomic::AtomicBool>,
}

impl IpcCapabilityRegistry {
    /// Bind a registry facade to an IPC bus. Both copies must be built with
    /// the same rendezvous dir to see each other (use
    /// [`IpcPlaneBus::with_rendezvous`]).
    pub fn new(bus: Arc<IpcPlaneBus>) -> Self {
        Self {
            bus,
            tools: Arc::new(RwLock::new(HashMap::new())),
            providers: Arc::new(RwLock::new(HashMap::new())),
            agents: Arc::new(RwLock::new(HashMap::new())),
            batch_depth: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            dirty_caps: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Coalesce bus route and capability manifest writes across `f` into one
    /// atomic replace per copy when the outermost batch exits.
    pub fn with_batch<R>(&self, f: impl FnOnce() -> R) -> R {
        self.begin_batch();
        let _guard = CapBatchGuard { reg: self };
        f()
    }

    fn begin_batch(&self) {
        self.bus.begin_batch();
        self.batch_depth
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    fn end_batch(&self) {
        self.bus.end_batch();
        let prev = self
            .batch_depth
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        if prev == 1
            && self
                .dirty_caps
                .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            self.flush_caps();
        }
    }

    fn flush_caps(&self) {
        let mut tools: Vec<(String, String)> = self
            .tools
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(name, t)| (name.clone(), t.description().to_string()))
            .collect();
        let mut providers: Vec<String> = self
            .providers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        let mut agents: Vec<AgentCapability> = self
            .agents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        let path = self.bus.caps_file();
        if tools.is_empty() && providers.is_empty() && agents.is_empty() {
            let _ = std::fs::remove_file(&path);
            return;
        }
        let ep = if !tools.is_empty() || !providers.is_empty() {
            self.endpoint_tag()
        } else {
            self.bus.bound_endpoint_tag()
        };
        tools.sort_by(|a, b| a.0.cmp(&b.0));
        providers.sort();
        agents.sort_by(|a, b| a.name.cmp(&b.name));
        let tool_vals: Vec<Value> = tools
            .into_iter()
            .map(|(name, description)| {
                json!({
                    "name": name,
                    "description": description,
                    "endpoint": ep,
                })
            })
            .collect();
        let provider_vals: Vec<Value> = providers
            .into_iter()
            .map(|name| {
                json!({
                    "name": name,
                    "endpoint": ep,
                })
            })
            .collect();
        let agent_vals: Vec<Value> = agents
            .into_iter()
            .map(|cap| {
                json!({
                    "name": cap.name,
                    "description": cap.description,
                    "is_core": cap.is_core,
                })
            })
            .collect();
        let manifest = json!({
            "endpoint": ep,
            "tool": tool_vals,
            "provider": provider_vals,
            "agent": agent_vals,
        });
        // Per-process endpoint metadata: a crash makes it stale whatever its
        // durability, so it is replaced atomically without fsync (one synced
        // write per tool held a first goal ~0.9 s on Windows).
        if let Ok(bytes) = serde_json::to_vec(&manifest) {
            let _ = crate::susi_config::atomic_replace_unsynced(&path, &bytes);
        }
    }

    fn mark_or_flush_caps(&self) {
        if self.batch_depth.load(std::sync::atomic::Ordering::Acquire) > 0 {
            self.dirty_caps
                .store(true, std::sync::atomic::Ordering::Release);
        } else {
            self.flush_caps();
        }
    }

    fn caps_dir(&self, kind: &str) -> PathBuf {
        self.bus.rendezvous().join("caps").join(kind)
    }

    /// First matching cap metadata across all rendezvous dirs (single-file
    /// `caps*.json` manifests first, then legacy `caps/<kind>/` files).
    fn find_cap(&self, kind: &str, name: &str) -> Option<Value> {
        for dir in self.bus.rendezvous_dirs() {
            for manifest in read_dir_cap_manifests(&dir) {
                if let Some(found) = manifest
                    .get(kind)
                    .and_then(Value::as_array)
                    .and_then(|arr| {
                        arr.iter()
                            .find(|m| m.get("name").and_then(Value::as_str) == Some(name))
                    })
                {
                    return Some(found.clone());
                }
            }
            if let Some(legacy) = read_cap(&dir.join("caps").join(kind), name) {
                return Some(legacy);
            }
        }
        None
    }

    fn scan_kind_caps(&self, kind: &str) -> Vec<Value> {
        let mut out = Vec::new();
        for dir in self.bus.rendezvous_dirs() {
            for manifest in read_dir_cap_manifests(&dir) {
                if let Some(arr) = manifest.get(kind).and_then(Value::as_array) {
                    out.extend(arr.iter().cloned());
                }
            }
            out.extend(scan_caps(&dir.join("caps").join(kind)));
        }
        out
    }

    /// Remove any legacy per-capability file only when it records this copy's
    /// endpoint — a same-named registration from another copy is left alone.
    fn remove_own_cap(&self, kind: &str, name: &str) {
        let file = self.caps_dir(kind).join(enc(name));
        if !file.exists() {
            return;
        }
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
        let tool: Arc<dyn Tool> = Arc::new(EnforcedTool {
            inner: Arc::new(tool),
        });
        self.tools
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.clone(), Arc::clone(&tool));
        self.bus
            .register(&tool_topic(&name), Arc::new(ToolDispatch { tool }));
        self.mark_or_flush_caps();
    }

    /// Local copy first; otherwise a remote proxy when another copy serves
    /// the capability (metadata file or wired topic).
    pub fn get_tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if let Some(t) = self
            .tools
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
        {
            return Some(Arc::clone(t));
        }
        let meta = self.find_cap("tool", name);
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
        let removed = self
            .tools
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(name)
            .is_some();
        self.bus.unregister(&tool_topic(name));
        if removed {
            self.mark_or_flush_caps();
        }
        self.remove_own_cap("tool", name);
        removed
    }

    /// All visible tool names — locally registered plus remote capabilities.
    pub fn list_tools(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .tools
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        for meta in self.scan_kind_caps("tool") {
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
        self.providers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.clone(), Arc::clone(&provider));
        self.bus.register(
            &provider_topic(&name),
            Arc::new(ProviderDispatch { provider }),
        );
        self.mark_or_flush_caps();
    }

    pub fn get_provider(&self, name: &str) -> Option<Arc<dyn Provider>> {
        if let Some(p) = self
            .providers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
        {
            return Some(Arc::clone(p));
        }
        if self.find_cap("provider", name).is_none() && !self.bus.is_wired(&provider_topic(name)) {
            return None;
        }
        Some(Arc::new(RemoteProvider {
            bus: Arc::clone(&self.bus),
            name: name.to_string(),
        }))
    }

    pub fn unregister_provider(&self, name: &str) -> bool {
        let removed = self
            .providers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(name)
            .is_some();
        self.bus.unregister(&provider_topic(name));
        if removed {
            self.mark_or_flush_caps();
        }
        self.remove_own_cap("provider", name);
        removed
    }

    pub fn list_providers(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .providers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        for meta in self.scan_kind_caps("provider") {
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
        self.agents
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name, cap);
        self.mark_or_flush_caps();
    }

    pub fn get_agent_capability(&self, name: &str) -> Option<AgentCapability> {
        if let Some(c) = self
            .agents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
        {
            return Some(c.clone());
        }
        let meta = self.find_cap("agent", name)?;
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
        let mut out: Vec<AgentCapability> = self
            .agents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for meta in self.scan_kind_caps("agent") {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (IpcCapabilityRegistry, IpcCapabilityRegistry) {
        let dir = std::env::temp_dir().join("susi-caps-test").join(format!(
            "{}-{}",
            std::process::id(),
            unique()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let a = IpcCapabilityRegistry::new(Arc::new(IpcPlaneBus::with_rendezvous(dir.clone())));
        let b = IpcCapabilityRegistry::new(Arc::new(IpcPlaneBus::with_rendezvous(dir)));
        (a, b)
    }

    fn unique() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    }

    struct EchoTool;

    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo_tool"
        }
        fn description(&self) -> &str {
            "echoes the args"
        }
        fn execute(&self, args: &Value, _workspace: &Path) -> EaiResult<String> {
            Ok(format!("echo:{args}"))
        }
    }

    struct EchoProvider;

    impl Provider for EchoProvider {
        fn name(&self) -> &str {
            "echo_prov"
        }
        fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
            Box::pin(async { Ok(true) })
        }
        fn generate(&self, prompt: &str) -> BoxFuture<'_, EaiResult<String>> {
            let p = prompt.to_string();
            Box::pin(async move { Ok(format!("gen:{p}")) })
        }
        fn embed(&self, text: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
            let t = text.len() as f32;
            Box::pin(async move { Ok(vec![t, 1.0]) })
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[test]
    fn remote_tool_registered_and_invoked() {
        let (a, b) = pair();
        a.register_tool(EchoTool);
        // registered copy serves locally
        assert!(a.get_tool("echo_tool").is_some());
        // other copy discovers + dispatches remotely
        let tool = b.get_tool("echo_tool").expect("remote tool");
        assert_eq!(tool.description(), "echoes the args");
        let out = tool
            .execute(&json!({"x": 1}), Path::new("."))
            .expect("remote execute");
        assert_eq!(out, "echo:{\"x\":1}");
        assert!(b.list_tools().contains(&"echo_tool".to_string()));
    }

    #[test]
    fn tool_error_propagates_to_remote_caller() {
        struct FailTool;
        impl Tool for FailTool {
            fn name(&self) -> &str {
                "fail_tool"
            }
            fn description(&self) -> &str {
                "always fails"
            }
            fn execute(&self, _args: &Value, _ws: &Path) -> EaiResult<String> {
                Err(EaiError::process("boom"))
            }
        }
        let (a, b) = pair();
        a.register_tool(FailTool);
        let tool = b.get_tool("fail_tool").expect("remote tool");
        let err = tool.execute(&json!({}), Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("boom"), "{err}");
    }

    #[test]
    fn unregister_tool_removes_remote_visibility() {
        let (a, b) = pair();
        a.register_tool(EchoTool);
        assert!(b.get_tool("echo_tool").is_some());
        a.unregister_tool("echo_tool");
        assert!(b.get_tool("echo_tool").is_none());
        // a stale proxy errors cleanly rather than hanging
        assert!(a.get_tool("echo_tool").is_none());
    }

    #[test]
    fn remote_provider_generate_and_embed() {
        let (a, b) = pair();
        a.register_provider(Arc::new(EchoProvider));
        let prov = b.get_provider("echo_prov").expect("remote provider");
        let out = block_on(prov.generate("hello")).expect("generate");
        assert_eq!(out, "gen:hello");
        let emb = block_on(prov.embed("abcd")).expect("embed");
        assert_eq!(emb, vec![4.0, 1.0]);
        assert!(block_on(prov.is_healthy()).expect("health"));
        assert!(b.list_providers().contains(&"echo_prov".to_string()));
    }

    #[test]
    fn agent_capability_visible_across_copies() {
        let (a, b) = pair();
        a.register_agent_capability(AgentCapability {
            name: "scout".into(),
            description: "recon".into(),
            is_core: true,
        });
        let cap = b.get_agent_capability("scout").expect("agent cap");
        assert!(cap.is_core);
        assert!(b.list_agents().iter().any(|c| c.name == "scout"));
        assert!(b.list_all_capabilities().iter().any(|n| n == "scout"));
    }

    #[test]
    fn with_batch_publishes_single_caps_and_routes_manifests() {
        struct NamedTool(String);
        impl Tool for NamedTool {
            fn name(&self) -> &str {
                &self.0
            }
            fn description(&self) -> &str {
                "batched tool"
            }
            fn execute(&self, args: &Value, _ws: &Path) -> EaiResult<String> {
                Ok(format!("{}:{args}", self.0))
            }
        }

        let bus_root = std::env::temp_dir()
            .join(format!(
                "susi-caps-batch-{}-{}",
                std::process::id(),
                unique()
            ))
            .join("bus");
        std::fs::create_dir_all(&bus_root).unwrap();
        let own = bus_root.join(std::process::id().to_string());
        let a_bus = Arc::new(IpcPlaneBus::with_rendezvous(own.clone()));
        let a = IpcCapabilityRegistry::new(Arc::clone(&a_bus));
        a.with_batch(|| {
            for idx in 0..32 {
                a.register_tool(NamedTool(format!("tool_{idx}")));
            }
            a.register_provider(Arc::new(EchoProvider));
            a.register_agent_capability(AgentCapability {
                name: "planner".into(),
                description: "plans".into(),
                is_core: true,
            });
            assert!(!a_bus.caps_file().exists());
            assert!(!a_bus.routes_file().exists());
        });
        assert_eq!(a_bus.caps_file(), own.join("caps.json"));
        assert_eq!(a_bus.routes_file(), own.join("routes.json"));
        assert!(own.join("caps.json").exists());
        assert!(own.join("routes.json").exists());
        assert!(!own.join("caps").exists());
        assert!(!own.join("topics").exists());

        let b = IpcCapabilityRegistry::new(Arc::new(IpcPlaneBus::with_rendezvous(
            bus_root.join("999998"),
        )));
        assert_eq!(b.list_tools().len(), 32);
        let remote = b.get_tool("tool_17").expect("batched remote tool");
        assert_eq!(
            remote.execute(&json!(5), Path::new(".")).unwrap(),
            "tool_17:5"
        );
        assert!(b.get_provider("echo_prov").is_some());
        assert!(b.get_agent_capability("planner").is_some());
        drop(b);
        drop(a);
        drop(a_bus);
        let _ = std::fs::remove_dir_all(bus_root.parent().unwrap());
    }
}
