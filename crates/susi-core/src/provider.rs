use crate::susi_error::EaiResult;
use std::any::Any;
use std::future::Future;
use std::pin::Pin;

/// A type alias for boxed futures to maintain object safety for the Provider trait.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The core Provider trait that abstracts model backends.
/// This allows pluggable inference engines (e.g., Candle, vLLM, Ollama, remote APIs)
/// while keeping the orchestration layer agnostic.
pub trait Provider: Send + Sync + 'static {
    /// The unique name of the provider (e.g., "Candle", "vLLM", "Ollama")
    fn name(&self) -> &str;

    /// Checks if the provider is healthy and ready to accept requests.
    fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>>;

    /// Generates text from the given prompt.
    fn generate(&self, prompt: &str) -> BoxFuture<'_, EaiResult<String>>;

    /// Generates embeddings for the given text.
    fn embed(&self, text: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>>;

    /// Exposes the underlying type for downcasting if provider-specific
    /// features are needed.
    fn as_any(&self) -> &dyn Any;
}

std::thread_local! {
    static ON_DEMAND_ENGINE_START: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Permission for provider calls made on this thread to start the local
/// inference engine they need (`ollama serve`, …) when it is installed but
/// not running.
///
/// Core starts nothing on its own (CEO decision 2026-11-03): only a
/// user-initiated request entry point — `susi run`, a CLI mission, the
/// `/v1` completion handlers — holds this guard. Unattended work (daemon
/// boot, the brain scout, the estate loop, scheduled missions) never does,
/// so a stopped engine is an ordinary transport error there. The guard is
/// thread-scoped and `!Send`: work handed to another thread or process does
/// not inherit it.
#[must_use = "the permission lasts only while the guard is alive"]
pub struct OnDemandEngineStart {
    _thread_bound: std::marker::PhantomData<*const ()>,
}

impl OnDemandEngineStart {
    /// Allow on-demand engine start for this thread until the guard drops.
    pub fn allow() -> Self {
        ON_DEMAND_ENGINE_START.with(|depth| depth.set(depth.get().saturating_add(1)));
        Self {
            _thread_bound: std::marker::PhantomData,
        }
    }

    /// True while a user-initiated request on this thread holds the guard.
    pub fn allowed() -> bool {
        ON_DEMAND_ENGINE_START.with(|depth| depth.get() > 0)
    }
}

impl Drop for OnDemandEngineStart {
    fn drop(&mut self) {
        ON_DEMAND_ENGINE_START.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

#[cfg(test)]
mod on_demand_engine_start_tests {
    use super::OnDemandEngineStart;

    #[test]
    fn the_permission_is_scoped_to_the_guard_and_its_thread() {
        assert!(!OnDemandEngineStart::allowed());
        {
            let _outer = OnDemandEngineStart::allow();
            let inner = OnDemandEngineStart::allow();
            assert!(OnDemandEngineStart::allowed());
            drop(inner);
            assert!(OnDemandEngineStart::allowed());
            let other_thread = std::thread::spawn(OnDemandEngineStart::allowed)
                .join()
                .unwrap();
            assert!(!other_thread, "another thread never inherits the guard");
        }
        assert!(!OnDemandEngineStart::allowed());
    }
}
