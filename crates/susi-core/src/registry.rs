use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::{Arc, OnceLock};

type Service = Arc<dyn Any + Send + Sync>;
type ServiceFactory = Arc<dyn Fn() -> Service + Send + Sync>;

/// Stores shared services by type and service factories by name.
#[derive(Default)]
pub struct DynamicServiceRegistry {
    services: RwLock<HashMap<TypeId, Service>>,
    factories: RwLock<HashMap<String, ServiceFactory>>,
}

impl DynamicServiceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a generic service statically resolvable by its TypeId.
    pub fn register<T: Send + Sync + 'static>(&self, service: T) {
        self.services
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(TypeId::of::<T>(), Arc::new(service));
    }

    /// Registers a factory by name, replacing any previous factory with that name.
    pub fn register_factory<T: Send + Sync + 'static, F>(&self, name: impl Into<String>, factory: F)
    where
        F: Fn() -> Arc<T> + Send + Sync + 'static,
    {
        self.factories
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
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
        let factory = Arc::clone(
            self.factories
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(name)?,
        );
        factory().downcast::<T>().ok()
    }

    /// Dynamically resolves and downcasts the provided generic type at runtime.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.services
            .read()
            .unwrap_or_else(|e| e.into_inner())
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
/// Delegates to [`crate::susi_core::registry_ipc::IpcCapabilityRegistry`] so capabilities
/// registered here are discoverable and invocable from every plane in the
/// process (shared `<cache>/bus/<pid>/` rendezvous). MAC
/// authorization + evidence capture are applied once at the tool boundary by
/// the IPC layer, for local and remote dispatch alike.
#[derive(Clone)]
pub struct CapabilityRegistry {
    ipc: crate::susi_core::registry_ipc::IpcCapabilityRegistry,
}

/// Lightweight agent capability mounted alongside providers and tools.
#[derive(Debug, Clone)]
pub struct AgentCapability {
    pub name: String,
    pub description: String,
    pub is_core: bool,
}

impl CapabilityRegistry {
    /// Fresh isolated catalog — fresh rendezvous dir, same semantics as the
    /// old per-instance in-memory maps (tests and unwired contexts rely on
    /// registrations not leaking between `new()` instances).
    pub fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("susi-reg-{}-{}", std::process::id(), nanos));
        Self {
            ipc: crate::susi_core::registry_ipc::IpcCapabilityRegistry::new(Arc::new(
                crate::susi_core::plane_bus_ipc::IpcPlaneBus::with_rendezvous(dir),
            )),
        }
    }

    /// Process-wide capability registry used by zero-config substrate
    /// discovery — shares the `<cache>/bus/<pid>/` rendezvous with every
    /// plane in this process.
    pub fn global() -> &'static Self {
        static INSTANCE: OnceLock<CapabilityRegistry> = OnceLock::new();
        INSTANCE.get_or_init(|| Self {
            ipc: crate::susi_core::registry_ipc::IpcCapabilityRegistry::new(
                crate::susi_core::plane_bus_ipc::IpcPlaneBus::global(),
            ),
        })
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

    /// Registers an abstract Tool with the capability registry. The IPC layer
    /// wraps it once with MAC authorization + evidence capture, so both local
    /// and remote dispatch enforce the execution boundary.
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
        self.ipc.list_agents().into_iter().map(|a| a.name).collect()
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

    /// Build the shared typed bus view of the registry's discovered entries.
    ///
    /// Existing registry kinds map to their owning surface; HTTP capabilities
    /// can register directly with [`crate::capability_bus::CapabilityBus`]
    /// because they are not necessarily local registry objects.
    #[must_use]
    pub fn typed_bus(&self) -> crate::capability_bus::CapabilityBus {
        use crate::capability_bus::{CapabilityBus, CapabilityDescriptor, CapabilitySurface};

        let mut bus = CapabilityBus::new();
        for (name, kind) in self.list_all_capabilities() {
            let surface = match kind {
                "agent" => CapabilitySurface::A2a,
                "tool" => CapabilitySurface::Mcp,
                "provider" => CapabilitySurface::Adapter,
                _ => continue,
            };
            let _ = bus.register(CapabilityDescriptor::new(
                name.clone(),
                surface,
                name,
                format!("capability:{kind}"),
                30_000,
                0,
            ));
        }
        bus
    }

    /// Dispatch one `Mcp`-surfaced capability through the shared typed bus:
    /// resolve the descriptor from the current discovery snapshot, then
    /// invoke the live registered tool (already MAC-enforced and
    /// evidence-captured by [`Self::register_tool`]) under the bus's
    /// permission/timeout/cost contract — the production caller
    /// `typed_bus()` otherwise lacked. It runs under a root delegation scope
    /// built from the request.
    pub fn dispatch_capability(
        &self,
        request: &crate::capability_bus::CapabilityRequest,
        args: &serde_json::Value,
        workspace: &std::path::Path,
    ) -> Result<crate::capability_bus::CapabilityReceipt, crate::capability_bus::CapabilityError>
    {
        let root = crate::capability_bus::CapabilityBus::root_scope(request)?;
        self.dispatch_capability_in(&root, request, args, workspace)
    }

    /// [`Self::dispatch_capability`] for a socket or CLI argument: with no
    /// `delegation` value (absent or JSON null) the call runs from a root
    /// scope; otherwise the value must be a `susi-delegation/v1` envelope
    /// from the calling driver and the call nests under it. The envelope is
    /// unsigned (`crate::delegation`): it bounds a cooperating driver, and
    /// the host MAC policy remains the authority.
    pub fn dispatch_nested(
        &self,
        delegation: Option<&serde_json::Value>,
        request: &crate::capability_bus::CapabilityRequest,
        args: &serde_json::Value,
        workspace: &std::path::Path,
    ) -> Result<crate::capability_bus::CapabilityReceipt, crate::capability_bus::CapabilityError>
    {
        use crate::capability_bus::CapabilityError;
        use crate::delegation::{DelegationEnvelope, DelegationError, DelegationScope};
        use serde::Deserialize;

        let Some(value) = delegation.filter(|value| !value.is_null()) else {
            return self.dispatch_capability(request, args, workspace);
        };
        let envelope = DelegationEnvelope::deserialize(value).map_err(|e| {
            CapabilityError::Delegation(DelegationError::InvalidEnvelope(e.to_string()))
        })?;
        let parent =
            DelegationScope::from_envelope(&envelope).map_err(CapabilityError::Delegation)?;
        self.dispatch_capability_in(&parent, request, args, workspace)
    }

    /// [`Self::dispatch_capability`] nested under `parent`, e.g. a scope a
    /// driver received as a [`crate::delegation::DelegationEnvelope`]: the
    /// invocation cannot exceed the parent's permissions, budget, deadline or
    /// depth, and its receipt carries the full delegation path.
    pub fn dispatch_capability_in(
        &self,
        parent: &crate::delegation::DelegationScope,
        request: &crate::capability_bus::CapabilityRequest,
        args: &serde_json::Value,
        workspace: &std::path::Path,
    ) -> Result<crate::capability_bus::CapabilityReceipt, crate::capability_bus::CapabilityError>
    {
        use crate::capability_bus::{CapabilityError, CapabilityInvocation, CapabilitySurface};

        let bus = self.typed_bus();
        let descriptor = bus
            .discover()
            .into_iter()
            .find(|d| d.id == request.capability_id)
            .ok_or_else(|| CapabilityError::UnknownCapability(request.capability_id.clone()))?;
        if descriptor.surface != CapabilitySurface::Mcp {
            return Err(CapabilityError::InvalidDescriptor(format!(
                "{:?}-surfaced capabilities cannot be dispatched yet; only mcp tools can",
                descriptor.surface
            )));
        }
        let tool = self
            .get_tool(&descriptor.id)
            .ok_or_else(|| CapabilityError::UnknownCapability(request.capability_id.clone()))?;
        bus.dispatch_in(parent, request, |_descriptor, _scope| {
            let started = std::time::Instant::now();
            let output = tool.execute(args, workspace).map_err(|e| e.to_string())?;
            Ok(CapabilityInvocation {
                output: serde_json::Value::String(output),
                elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                cost_micros: 0,
            })
        })
    }
}

impl Default for CapabilityRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn agents_tools_and_providers_mount_as_capabilities() {
        let registry = CapabilityRegistry::new();
        registry.register_agent_capability(AgentCapability {
            name: "SafetyAgent".into(),
            description: "governance".into(),
            is_core: true,
        });
        struct T;
        impl Tool for T {
            fn name(&self) -> &str {
                "probe"
            }
            fn description(&self) -> &str {
                "probe tool"
            }
            fn execute(
                &self,
                _args: &serde_json::Value,
                _workspace: &std::path::Path,
            ) -> crate::susi_error::EaiResult<String> {
                Ok("ok".into())
            }
        }
        registry.register_tool(T);
        let all = registry.list_all_capabilities();
        assert!(all.iter().any(|(n, k)| n == "SafetyAgent" && *k == "agent"));
        assert!(all.iter().any(|(n, k)| n == "probe" && *k == "tool"));
        assert!(registry.get_agent("SafetyAgent").unwrap().is_core);
    }

    #[test]
    fn factory_can_replace_itself_without_deadlocking() {
        let registry = Arc::new(DynamicServiceRegistry::new());
        let weak = Arc::downgrade(&registry);
        registry.register_factory("number", move || {
            weak.upgrade()
                .unwrap()
                .register_factory("number", || Arc::new(2_u32));
            Arc::new(1_u32)
        });
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            assert_eq!(*registry.instantiate::<u32>("number").unwrap(), 1);
            assert_eq!(*registry.instantiate::<u32>("number").unwrap(), 2);
            tx.send(()).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("factory deadlocked");
    }

    #[test]
    fn services_and_factories_preserve_type_checks() {
        let registry = DynamicServiceRegistry::new();
        registry.register(7_u32);
        assert_eq!(*registry.get::<u32>().unwrap(), 7);
        assert!(registry.get::<String>().is_none());
        registry.register_factory("number", || Arc::new(9_u32));
        assert!(registry.instantiate::<String>("number").is_none());
        assert!(registry.instantiate::<u32>("missing").is_none());
    }

    #[test]
    fn capability_registry_unregister_and_typed_bus_surface_mapping_contract() {
        use crate::capability_bus::CapabilitySurface;

        let registry = CapabilityRegistry::new();
        registry.register_agent_capability(AgentCapability {
            name: "PlannerAgent".into(),
            description: "planner".into(),
            is_core: false,
        });
        struct EchoTool;
        impl Tool for EchoTool {
            fn name(&self) -> &str {
                "echo_tool"
            }
            fn description(&self) -> &str {
                "echo"
            }
            fn execute(
                &self,
                _args: &serde_json::Value,
                _workspace: &std::path::Path,
            ) -> crate::susi_error::EaiResult<String> {
                Ok("echo".into())
            }
        }
        registry.register_tool(EchoTool);

        let discovered = registry.typed_bus().discover();
        assert!(discovered
            .iter()
            .any(|d| d.id == "PlannerAgent" && d.surface == CapabilitySurface::A2a));
        assert!(discovered
            .iter()
            .any(|d| d.id == "echo_tool" && d.surface == CapabilitySurface::Mcp));

        assert!(registry.unregister_tool("echo_tool"));
        assert!(!registry.unregister_tool("echo_tool"));
    }

    #[test]
    fn dispatch_capability_nests_a_live_tool_under_a_received_envelope() {
        use crate::capability_bus::{CapabilityError, CapabilityRequest};
        use crate::delegation::{
            DelegationError, DelegationHop, DelegationLimits, DelegationRequest, DelegationScope,
        };

        let registry = CapabilityRegistry::new();
        struct EchoTool;
        impl Tool for EchoTool {
            fn name(&self) -> &str {
                "echo_tool"
            }
            fn description(&self) -> &str {
                "echo"
            }
            fn execute(
                &self,
                _args: &serde_json::Value,
                _workspace: &std::path::Path,
            ) -> crate::susi_error::EaiResult<String> {
                Ok("echo".into())
            }
        }
        registry.register_tool(EchoTool);
        let ws = std::path::Path::new(".");
        let args = serde_json::json!({});

        // A driver on one side of the socket opens its scope and sends it.
        let session = DelegationScope::root(
            DelegationHop::new("ceo-session", "root").unwrap(),
            ["capability:tool".to_string()],
            1_000,
            Some(60_000),
            DelegationLimits::default(),
        )
        .unwrap();
        let driver = session
            .delegate(DelegationRequest {
                hop: DelegationHop::new("dot", "plan").unwrap(),
                permissions: ["capability:tool".to_string()].into(),
                budget_micros: 100,
                timeout_ms: 30_000,
            })
            .unwrap();
        let received = DelegationScope::from_envelope(&driver.envelope()).unwrap();

        let request =
            CapabilityRequest::new("echo_tool", "opus", ["capability:tool".into()], 5_000, 50);
        let receipt = registry
            .dispatch_capability_in(&received, &request, &args, ws)
            .unwrap();
        assert_eq!(receipt.output, serde_json::json!("echo"));
        let path: Vec<String> = receipt
            .delegation_path
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            path,
            ["ceo-session:root", "dot:plan", "opus:echo_tool"].map(String::from)
        );

        // Without the envelope the same call is a root-level dispatch.
        let top = registry.dispatch_capability(&request, &args, ws).unwrap();
        assert_eq!(top.delegation_path.len(), 2);

        // Permissions the envelope does not carry cannot be presented.
        let greedy = CapabilityRequest::new(
            "echo_tool",
            "opus",
            ["capability:tool".into(), "admin".into()],
            5_000,
            50,
        );
        assert!(matches!(
            registry.dispatch_capability_in(&received, &greedy, &args, ws),
            Err(CapabilityError::Delegation(
                DelegationError::PermissionAmplification { .. }
            ))
        ));
        received.cancel();
        assert!(matches!(
            registry.dispatch_capability_in(&received, &request, &args, ws),
            Err(CapabilityError::Delegation(DelegationError::Cancelled(_)))
        ));
    }

    #[test]
    fn provider_register_list_get_and_unregister_lifecycle_works() {
        use crate::susi_core::provider::BoxFuture;
        struct StubProvider;
        impl Provider for StubProvider {
            fn name(&self) -> &str {
                "stub-llm"
            }
            fn is_healthy(&self) -> BoxFuture<'_, crate::susi_error::EaiResult<bool>> {
                Box::pin(async { Ok(true) })
            }
            fn generate(
                &self,
                _prompt: &str,
            ) -> BoxFuture<'_, crate::susi_error::EaiResult<String>> {
                Box::pin(async { Ok("stub".into()) })
            }
            fn embed(&self, _text: &str) -> BoxFuture<'_, crate::susi_error::EaiResult<Vec<f32>>> {
                Box::pin(async { Ok(vec![1.0]) })
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
        }

        let registry = CapabilityRegistry::default();
        registry.register_provider(StubProvider);
        assert!(registry.list_providers().contains(&"stub-llm".to_string()));
        assert_eq!(
            registry.get_provider("stub-llm").unwrap().name(),
            "stub-llm"
        );
        assert!(registry.unregister_provider("stub-llm"));
        assert!(!registry.unregister_provider("stub-llm"));
    }
}
