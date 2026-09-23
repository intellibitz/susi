//! Vendored `registry` — same public API as `susi_core::registry`, but
//! `CapabilityRegistry` delegates to
//! [`crate::susi_core::registry_ipc::IpcCapabilityRegistry`] so capabilities
//! registered in one vendored copy are discoverable and invocable from every
//! other copy in the process. `DynamicServiceRegistry` stays local: TypeId
//! keys are per-copy types by construction.

use crate::susi_core::plane_bus_ipc::IpcPlaneBus;
use crate::susi_core::registry_ipc::IpcCapabilityRegistry;
use dashmap::DashMap;
use std::any::{Any, TypeId};
use std::sync::{Arc, OnceLock};

type Service = Arc<dyn Any + Send + Sync>;
type ServiceFactory = Arc<dyn Fn() -> Service + Send + Sync>;

/// Stores shared services by type and service factories by name.
#[derive(Default)]
pub struct DynamicServiceRegistry {
    services: DashMap<TypeId, Service>,
    factories: DashMap<String, ServiceFactory>,
}

impl DynamicServiceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a generic service statically resolvable by its TypeId.
    pub fn register<T: Send + Sync + 'static>(&self, service: T) {
        self.services.insert(TypeId::of::<T>(), Arc::new(service));
    }

    /// Registers a factory by name, replacing any previous factory with that name.
    pub fn register_factory<T: Send + Sync + 'static, F>(&self, name: impl Into<String>, factory: F)
    where
        F: Fn() -> Arc<T> + Send + Sync + 'static,
    {
        self.factories.insert(
            name.into(),
            Arc::new(move || {
                let instance: Arc<T> = factory();
                instance as Arc<dyn Any + Send + Sync>
            }),
        );
    }

    /// Instantiates a generic service dynamically via its registered String semantic identifier.
    pub fn instantiate<T: Send + Sync + 'static>(&self, name: &str) -> Option<Arc<T>> {
        // Release the map guard before calling user code, which may register factories.
        let factory = Arc::clone(self.factories.get(name)?.value());
        factory().downcast::<T>().ok()
    }

    /// Dynamically resolves and downcasts the provided generic type at runtime.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.services
            .get(&TypeId::of::<T>())
            .and_then(|val| val.clone().downcast::<T>().ok())
    }
}

use crate::susi_core::provider::Provider;

/// An abstract Tool that can be invoked dynamically.
pub trait Tool: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn execute(
        &self,
        args: &serde_json::Value,
        workspace: &std::path::Path,
    ) -> crate::susi_error::EaiResult<String>;
}

/// A specialized registry for managing capabilities (providers, tools, agents)
/// in the susi ecosystem. This acts as the central router for dynamic discovery.
///
/// Vendored: capability state lives in the shared `<cache>/bus/<pid>/`
/// rendezvous via [`IpcCapabilityRegistry`] — `get_tool` returns a remote
/// proxy for capabilities owned by other copies, and locally registered
/// tools are served to remote copies through `capability.tool.<name>` bus
/// topics with MAC + evidence capture enforced at the owner boundary.
pub struct CapabilityRegistry {
    ipc: IpcCapabilityRegistry,
}

/// Lightweight agent capability mounted alongside providers and tools.
#[derive(Debug, Clone)]
pub struct AgentCapability {
    pub name: String,
    pub description: String,
    pub is_core: bool,
}

impl CapabilityRegistry {
    pub fn new() -> Self {
        Self {
            ipc: IpcCapabilityRegistry::new(IpcPlaneBus::global()),
        }
    }

    /// Process-wide capability registry used by zero-config substrate discovery.
    pub fn global() -> &'static Self {
        static INSTANCE: OnceLock<CapabilityRegistry> = OnceLock::new();
        INSTANCE.get_or_init(Self::new)
    }

    /// Registers a model provider with the capability registry.
    pub fn register_provider<P: Provider + 'static>(&self, provider: P) {
        self.ipc.register_provider(Arc::new(provider));
    }

    /// Retrieves a provider by name (local or remote proxy).
    pub fn get_provider(&self, name: &str) -> Option<Arc<dyn Provider>> {
        self.ipc.get_provider(name)
    }

    /// Retrieves a list of all registered provider names.
    pub fn list_providers(&self) -> Vec<String> {
        self.ipc.list_providers()
    }

    /// Removes a provider by name. Returns true if it was present.
    pub fn unregister_provider(&self, name: &str) -> bool {
        self.ipc.unregister_provider(name)
    }

    /// Registers an abstract Tool with the capability registry. The tool is
    /// wrapped with MAC authorization + evidence capture at registration, so
    /// both local and remote dispatch enforce the execution boundary.
    pub fn register_tool<T: Tool + 'static>(&self, tool: T) {
        self.ipc.register_tool(tool);
    }

    /// Retrieves a tool by name (local or remote proxy).
    pub fn get_tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.ipc.get_tool(name)
    }

    /// Removes a tool by name. Returns true if it was present locally.
    pub fn unregister_tool(&self, name: &str) -> bool {
        self.ipc.unregister_tool(name)
    }

    /// Retrieves a list of all registered tool names.
    pub fn list_tools(&self) -> Vec<String> {
        self.ipc.list_tools()
    }

    /// Mount an agent capability (name/description) into the same registry as
    /// providers and tools — one catalog for models, agents, and MCP tools.
    pub fn register_agent_capability(&self, agent: AgentCapability) {
        self.ipc.register_agent_capability(agent);
    }

    pub fn get_agent(&self, name: &str) -> Option<AgentCapability> {
        self.ipc.get_agent_capability(name)
    }

    pub fn list_agents(&self) -> Vec<String> {
        self.ipc
            .list_agents()
            .into_iter()
            .map(|a| a.name)
            .collect()
    }

    /// Unified capability inventory: providers + tools + agents.
    pub fn list_all_capabilities(&self) -> Vec<(String, &'static str)> {
        let mut out = Vec::new();
        for name in self.list_providers() {
            out.push((name, "provider"));
        }
        for name in self.list_tools() {
            out.push((name, "tool"));
        }
        for name in self.list_agents() {
            out.push((name, "agent"));
        }
        out
    }
}

impl Default for CapabilityRegistry {
    fn default() -> Self {
        Self::new()
    }
}
