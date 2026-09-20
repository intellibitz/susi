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

use crate::provider::Provider;

/// An abstract Tool that can be invoked dynamically.
pub trait Tool: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn execute(
        &self,
        args: &serde_json::Value,
        workspace: &std::path::Path,
    ) -> susi_error::EaiResult<String>;
}

/// A specialized registry for managing capabilities (providers, tools, agents)
/// in the susi ecosystem. This acts as the central router for dynamic discovery.
#[derive(Default, Clone)]
pub struct CapabilityRegistry {
    /// Underlying generic registry for holding capability instances
    services: Arc<DynamicServiceRegistry>,
    /// Maps provider names to their instantiated capabilities
    providers: Arc<DashMap<String, Arc<dyn Provider>>>,
    /// Maps tool names to their instantiated capabilities
    tools: Arc<DashMap<String, Arc<dyn Tool>>>,
}

impl CapabilityRegistry {
    pub fn new() -> Self {
        Self {
            services: Arc::new(DynamicServiceRegistry::new()),
            providers: Arc::new(DashMap::new()),
            tools: Arc::new(DashMap::new()),
        }
    }

    /// Process-wide capability registry used by zero-config substrate discovery.
    pub fn global() -> &'static Self {
        static INSTANCE: OnceLock<CapabilityRegistry> = OnceLock::new();
        INSTANCE.get_or_init(Self::new)
    }

    /// Registers a model provider with the capability registry.
    pub fn register_provider<P: Provider + 'static>(&self, provider: P) {
        let name = provider.name().to_string();
        self.providers.insert(name, Arc::new(provider));
    }

    /// Retrieves a provider by name.
    pub fn get_provider(&self, name: &str) -> Option<Arc<dyn Provider>> {
        self.providers.get(name).map(|v| v.clone())
    }

    /// Retrieves a list of all registered provider names.
    pub fn list_providers(&self) -> Vec<String> {
        self.providers.iter().map(|kv| kv.key().clone()).collect()
    }

    /// Registers an abstract Tool with the capability registry.
    pub fn register_tool<T: Tool + 'static>(&self, tool: T) {
        let name = tool.name().to_string();
        self.tools.insert(name, Arc::new(tool));
    }

    /// Retrieves a tool by name.
    pub fn get_tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).map(|v| v.clone())
    }

    /// Retrieves a list of all registered tool names.
    pub fn list_tools(&self) -> Vec<String> {
        self.tools.iter().map(|kv| kv.key().clone()).collect()
    }

    /// Exposes the underlying dynamic service registry for ad-hoc capability registration.
    pub fn dynamic_services(&self) -> &DynamicServiceRegistry {
        &self.services
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
}
