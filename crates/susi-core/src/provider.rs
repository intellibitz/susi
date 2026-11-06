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

    /// Re-bind a caller thread's user-request permission on a plane-bus worker
    /// thread when the request payload explicitly carried
    /// `"on_demand_engine_start": true`.
    pub fn from_bus_flag(allowed: bool) -> Option<Self> {
        allowed.then(Self::allow)
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
    use super::{BoxFuture, EaiResult, OnDemandEngineStart, Provider};
    use std::any::Any;

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

    #[test]
    fn on_demand_engine_start_starts_disallowed_on_fresh_thread() {
        let allowed = std::thread::spawn(OnDemandEngineStart::allowed)
            .join()
            .unwrap();
        assert!(!allowed);
    }

    #[test]
    fn on_demand_engine_start_multiple_sequential_cycles_reset_cleanly() {
        for _ in 0..2 {
            {
                let _g = OnDemandEngineStart::allow();
                assert!(OnDemandEngineStart::allowed());
            }
            assert!(!OnDemandEngineStart::allowed());
        }
    }

    #[test]
    fn from_bus_flag_rebinds_permission_on_worker_thread_only_when_true() {
        let (false_seen, true_seen, after_drop) = std::thread::spawn(|| {
            let none_guard = OnDemandEngineStart::from_bus_flag(false);
            assert!(none_guard.is_none());
            let false_seen = OnDemandEngineStart::allowed();
            let true_seen = {
                let some_guard = OnDemandEngineStart::from_bus_flag(true);
                assert!(some_guard.is_some());
                OnDemandEngineStart::allowed()
            };
            let after_drop = OnDemandEngineStart::allowed();
            (false_seen, true_seen, after_drop)
        })
        .join()
        .unwrap();
        assert!(!false_seen);
        assert!(true_seen);
        assert!(!after_drop);
    }

    #[test]
    fn mock_provider_trait_object_is_object_safe_and_downcastable() {
        struct DummyProvider;
        impl Provider for DummyProvider {
            fn name(&self) -> &str {
                "dummy"
            }
            fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
                Box::pin(async { Ok(true) })
            }
            fn generate(&self, _prompt: &str) -> BoxFuture<'_, EaiResult<String>> {
                Box::pin(async { Ok("ok".into()) })
            }
            fn embed(&self, _text: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
                Box::pin(async { Ok(vec![1.0]) })
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
        }

        let p: Box<dyn Provider> = Box::new(DummyProvider);
        assert_eq!(p.name(), "dummy");
        assert!(p.as_any().downcast_ref::<DummyProvider>().is_some());
    }

    #[test]
    fn dummy_provider_async_methods_resolve_expected_health_generation_and_embeddings() {
        struct ReadyProvider;
        impl Provider for ReadyProvider {
            fn name(&self) -> &str {
                "ready"
            }
            fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
                Box::pin(async { Ok(true) })
            }
            fn generate(&self, prompt: &str) -> BoxFuture<'_, EaiResult<String>> {
                let out = format!("echo:{prompt}");
                Box::pin(async move { Ok(out) })
            }
            fn embed(&self, _text: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
                Box::pin(async { Ok(vec![0.5, -0.5]) })
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
        }

        let prov = ReadyProvider;
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);

        let mut h = prov.is_healthy();
        assert!(matches!(
            h.as_mut().poll(&mut cx),
            std::task::Poll::Ready(Ok(true))
        ));

        let mut g = prov.generate("ping");
        assert!(matches!(
            g.as_mut().poll(&mut cx),
            std::task::Poll::Ready(Ok(ref s)) if s == "echo:ping"
        ));

        let mut e = prov.embed("ping");
        assert!(matches!(
            e.as_mut().poll(&mut cx),
            std::task::Poll::Ready(Ok(ref v)) if v == &[0.5, -0.5]
        ));
    }
}
