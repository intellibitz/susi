//! Availability never poisons the capability ranking (VC-202-020,
//! T-DEEPSEEK-117) — proven by refutation on both axes.
//!
//! A model hammered by rate limits, timeouts and 5xx errors is having an
//! outage, not a bad-brain episode: those failures are health evidence
//! (cooldowns, quarantine, `last_failure`), and the capability record —
//! samples, success rate, cost per verified outcome — must not move. The
//! inverse must also hold or the proof is vacuous: a genuinely wrong or
//! unusable answer DOES move the capability ranking.

use crate::engines::brain::{FailureKind, Store, TaskClass};
use crate::engines::runtime::GemiEngine;
use crate::susi_core::provider::{BoxFuture, Provider};
use crate::susi_core::registry::CapabilityRegistry;
use crate::susi_core::susi_error::{EaiError, EaiResult};

#[test]
fn availability_not_capability_storm_leaves_the_ranking_untouched() {
    // The capability axis: 5 verified successes → rate 1.0.
    let mut s = Store::default();
    for _ in 0..5 {
        s.record("acme-av-a", TaskClass::Chat, true, 100);
    }
    let before = s.rank(&["acme-av-a".to_string()], TaskClass::Chat)[0].clone();
    assert_eq!(before.samples, 5);
    assert_eq!(before.success_rate, Some(1.0));

    // The availability storm: 20 transport-class failures in a row land
    // ONLY on the health axis (FailureKind + streak), exactly as
    // `InferenceRouter::record_failure` feeds it on the dispatch path.
    for _ in 0..20 {
        s.note_failure("acme-av-a", FailureKind::RateLimit, 1_800_000_000);
        s.note_failure("acme-av-a", FailureKind::Transport, 1_800_000_001);
    }
    let after = s.rank(&["acme-av-a".to_string()], TaskClass::Chat)[0].clone();
    assert_eq!(after.samples, before.samples, "no fake capability samples");
    assert_eq!(
        after.success_rate, before.success_rate,
        "capability ranking unchanged by the availability storm"
    );
    assert_eq!(
        after.cost_per_outcome_usd, before.cost_per_outcome_usd,
        "cost per verified outcome is a capability number — it cannot move"
    );
    // The health axis absorbed the storm: the most recent failure is
    // visible on the ranked record — the outage is recorded, not ignored.
    assert_eq!(
        after.last_failure,
        Some((FailureKind::Transport, 40)),
        "the storm's 40 failures live on the health axis, not the record"
    );
    // And it did not mark the provider unfit by itself: fitness needs
    // genuinely poor capability evidence plus a live failure.
    assert!(!after.unfit, "a healthy-but-flaky model is not unfit");
}

#[test]
fn availability_not_capability_wrong_answers_move_the_ranking() {
    // The inverse: genuinely failed answers MUST reorder — otherwise the
    // refutation above proves nothing.
    let mut s = Store::default();
    for _ in 0..4 {
        s.record("acme-av-b", TaskClass::Chat, true, 100);
        s.record("acme-av-c", TaskClass::Chat, true, 100);
    }
    let names = vec!["acme-av-b".to_string(), "acme-av-c".to_string()];
    // Tie evidence: alphabetical/static order decides (b first).
    assert_eq!(s.rank(&names, TaskClass::Chat)[0].provider, "acme-av-b");
    // A genuinely wrong/unusable answer counts against capability.
    for _ in 0..4 {
        s.record("acme-av-b", TaskClass::Chat, false, 0);
    }
    let ranked = s.rank(&names, TaskClass::Chat);
    assert_eq!(
        ranked[0].provider, "acme-av-c",
        "wrong answers reorder the capability ranking — the axis is real"
    );
    assert_eq!(ranked[1].success_rate, Some(0.5));
}

/// A provider whose calls always fail at the transport layer — a 5xx/
/// rate-limit outage, not a bad answer.
struct FailingProvider {
    name: &'static str,
    error: &'static str,
}

impl Provider for FailingProvider {
    fn name(&self) -> &str {
        self.name
    }
    fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
        Box::pin(async { Ok(true) })
    }
    fn generate(&self, _prompt: &str) -> BoxFuture<'_, EaiResult<String>> {
        let error = EaiError::process(self.error);
        Box::pin(async move { Err(error) })
    }
    fn embed(&self, _text: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[test]
fn availability_not_capability_dispatch_storm_stays_off_capability() {
    // End to end on the production cascade: the provider fails every call
    // with a 5xx-class error; its capability samples stay at zero while
    // the health axis (cooldown) absorbs the failure.
    let _env = crate::engines::env_test_lock();
    let evidence = std::env::temp_dir().join(format!("susi-av-{}", std::process::id()));
    let _ = std::fs::remove_file(&evidence);
    // SAFETY: serialized by env_test_lock; restored before drop.
    unsafe {
        std::env::set_var("SUSI_BRAIN_EVIDENCE_FILE", &evidence);
    }
    let _ = crate::engines::brain::reset();

    let registry = CapabilityRegistry::new();
    registry.register_provider(FailingProvider {
        name: "avstorm-flaky",
        error: "http 500 internal error",
    });
    registry.register_provider(FailingProvider {
        name: "avstorm-flaky2",
        error: "connection refused",
    });
    let (out, _ladder) =
        GemiEngine::try_providers(&registry, "ping availability", None, None, &|_| {}, &|_| {});
    assert_eq!(out, None, "every candidate failed at transport");

    let store = crate::engines::brain::load();
    for provider in ["avstorm-flaky", "avstorm-flaky2"] {
        let ranked = store.rank(&[provider.to_string()], TaskClass::Reflex);
        assert_eq!(
            ranked[0].samples, 0,
            "{provider}: transport failures record no capability samples"
        );
        assert_eq!(ranked[0].success_rate, None);
        assert!(
            ranked[0].last_failure.is_some(),
            "{provider}: the health axis absorbed the failure"
        );
        assert!(
            crate::engines::routing::InferenceRouter::provider_cooled(provider),
            "{provider}: cooled by the health axis"
        );
    }

    unsafe {
        std::env::remove_var("SUSI_BRAIN_EVIDENCE_FILE");
    }
    let _ = crate::engines::brain::reset();
}

#[test]
fn availability_not_capability_bad_answer_still_records() {
    // The same dispatch, with the provider ANSWERING but unusably (empty
    // reply): a real response with nothing in it is capability evidence —
    // it records a failure on the (provider, class) record.
    let _env = crate::engines::env_test_lock();
    let evidence = std::env::temp_dir().join(format!("susi-avb-{}", std::process::id()));
    let _ = std::fs::remove_file(&evidence);
    // SAFETY: serialized by env_test_lock; restored before drop.
    unsafe {
        std::env::set_var("SUSI_BRAIN_EVIDENCE_FILE", &evidence);
    }
    let _ = crate::engines::brain::reset();

    struct EmptyProvider;
    impl Provider for EmptyProvider {
        fn name(&self) -> &str {
            "avstorm-empty"
        }
        fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
            Box::pin(async { Ok(true) })
        }
        fn generate(&self, _prompt: &str) -> BoxFuture<'_, EaiResult<String>> {
            Box::pin(async { Ok(String::new()) })
        }
        fn embed(&self, _text: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }
    let registry = CapabilityRegistry::new();
    registry.register_provider(EmptyProvider);
    let _ = GemiEngine::try_providers(&registry, "ping availability", None, None, &|_| {}, &|_| {});
    let store = crate::engines::brain::load();
    let ranked = store.rank(&["avstorm-empty".to_string()], TaskClass::Reflex);
    assert_eq!(
        ranked[0].samples, 1,
        "a reached-but-empty answer is capability evidence — it counts"
    );
    assert_eq!(ranked[0].success_rate, Some(0.0));

    unsafe {
        std::env::remove_var("SUSI_BRAIN_EVIDENCE_FILE");
    }
    let _ = crate::engines::brain::reset();
}

#[test]
fn availability_not_capability_production_wiring() {
    // The axis split must live on the real dispatch path, not in test
    // helpers: outcomes are recorded only when the model actually
    // produced a response.
    let runtime_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/engines/runtime.rs"
    ))
    .expect("runtime.rs readable");
    assert!(
        runtime_src.contains("if outcome.is_ok()"),
        "capability evidence only records real answers"
    );
    assert!(
        runtime_src.contains("record_failure(&name, &msg)"),
        "Err goes to the health axis via record_failure"
    );
}
