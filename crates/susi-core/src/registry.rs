use dashmap::DashMap;
use std::any::{Any, TypeId};
use std::sync::Arc;

/// An innovative dynamic Service Locator mapping `TypeId` to generic instances.
#[derive(Default)]
pub struct DynamicServiceRegistry {
    services: DashMap<TypeId, Arc<dyn Any + Send + Sync>>,
    factories: DashMap<String, Box<dyn Fn() -> Arc<dyn Any + Send + Sync> + Send + Sync>>,
}

impl DynamicServiceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a generic service statically resolvable by its TypeId.
    pub fn register<T: Send + Sync + 'static>(&self, service: T) {
        self.services.insert(TypeId::of::<T>(), Arc::new(service));
    }

    /// Registers a generic factory tied to a dynamic String semantic identifier.
    /// This introduces String-based Innovative Dynamism decoupled from compile-time Enums.
    pub fn register_factory<T: Send + Sync + 'static, F>(&self, name: impl Into<String>, factory: F)
    where
        F: Fn() -> Arc<T> + Send + Sync + 'static,
    {
        self.factories.insert(
            name.into(),
            Box::new(move || {
                let instance: Arc<T> = factory();
                instance as Arc<dyn Any + Send + Sync>
            }),
        );
    }

    /// Instantiates a generic service dynamically via its registered String semantic identifier.
    pub fn instantiate<T: Send + Sync + 'static>(&self, name: &str) -> Option<Arc<T>> {
        if let Some(fac) = self.factories.get(name) {
            let instance_any = fac();
            instance_any.downcast::<T>().ok()
        } else {
            None
        }
    }

    /// Dynamically resolves and downcasts the provided generic type at runtime.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.services
            .get(&TypeId::of::<T>())
            .and_then(|val| val.clone().downcast::<T>().ok())
    }
}
