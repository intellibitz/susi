use dashmap::DashMap;
use std::any::{Any, TypeId};
use std::sync::Arc;

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
