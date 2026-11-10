//! `/v1/embeddings` answers within one budget, whatever the providers do
//! (T-CLAUDEGEMIFIX-1). It used to try every registered provider with a 60 s
//! call each and no overall bound, and did not answer within 300 s.

use crate::engines::runtime::GemiEngine;
use crate::susi_core::provider::{BoxFuture, Provider};
use crate::susi_core::registry::CapabilityRegistry;
use crate::susi_core::susi_error::{EaiError, EaiResult};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

enum Embeds {
    Never,
    Fails,
    Vector,
}

struct Fake(&'static str, Embeds);

impl Provider for Fake {
    fn name(&self) -> &str {
        self.0
    }
    fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
        Box::pin(async { Ok(true) })
    }
    fn generate(&self, _prompt: &str) -> BoxFuture<'_, EaiResult<String>> {
        Box::pin(async { Ok(String::new()) })
    }
    fn embed(&self, _text: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
        match self.1 {
            Embeds::Never => Box::pin(std::future::pending()),
            Embeds::Fails => Box::pin(async { Err(EaiError::network("unreachable")) }),
            Embeds::Vector => Box::pin(async { Ok(vec![0.5, 0.25]) }),
        }
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

struct CountedFailure {
    calls: Arc<AtomicUsize>,
}

impl Provider for CountedFailure {
    fn name(&self) -> &str {
        "a-fails"
    }

    fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
        Box::pin(async { Ok(true) })
    }

    fn generate(&self, _prompt: &str) -> BoxFuture<'_, EaiResult<String>> {
        Box::pin(async { Ok(String::new()) })
    }

    fn embed(&self, _text: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(EaiError::network("unreachable")) })
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[test]
fn a_provider_that_never_answers_cannot_hold_the_request_past_its_budget() {
    let registry = CapabilityRegistry::new();
    registry.register_provider(Fake("a-hangs", Embeds::Never));
    registry.register_provider(Fake("b-hangs-too", Embeds::Never));
    let started = Instant::now();
    let error =
        GemiEngine::embed_from(&registry, "hello", None, Duration::from_millis(300)).unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert!(error.contains("no embedding within the budget"), "{error}");
}

#[test]
fn providers_are_tried_in_turn_and_a_named_one_first() {
    let registry = CapabilityRegistry::new();
    registry.register_provider(Fake("a-fails", Embeds::Fails));
    registry.register_provider(Fake("b-embeds", Embeds::Vector));
    let budget = Duration::from_secs(5);
    assert_eq!(
        GemiEngine::embed_from(&registry, "x", None, budget).unwrap(),
        [0.5, 0.25]
    );
    let only_failing = CapabilityRegistry::new();
    only_failing.register_provider(Fake("a-fails", Embeds::Fails));
    let error = GemiEngine::embed_from(&only_failing, "x", Some("a-fails"), budget).unwrap_err();
    assert!(error.contains("a-fails: "), "{error}");
}

#[test]
fn a_named_provider_is_not_retried_after_the_preferred_attempt() {
    let calls = Arc::new(AtomicUsize::new(0));
    let registry = CapabilityRegistry::new();
    registry.register_provider(CountedFailure {
        calls: Arc::clone(&calls),
    });
    registry.register_provider(Fake("b-embeds", Embeds::Vector));

    assert_eq!(
        GemiEngine::embed_from(&registry, "x", Some("a-fails"), Duration::from_secs(5)).unwrap(),
        [0.5, 0.25]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
