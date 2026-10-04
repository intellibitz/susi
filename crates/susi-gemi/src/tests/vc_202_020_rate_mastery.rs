//! Mastery verification for the arbitration half of VC-202-020
//! (T-DEEPSEEK-116): per-key rate and concurrency limits are arbitrated
//! across everything running in this process.
//!
//! `key_arbitration` is a process-global arbiter that every production
//! `provider.generate` call in susi-gemi admits through before the request
//! is issued: the provider-failover cascade in `engines/runtime.rs` and the
//! one-shot probe helpers (`coding_models_ext`, `frontier_ext`,
//! `open_weight_ext`, `openrouter_ext`). A saturated key is refused
//! non-blockingly — the cascade moves to the next provider, the probe
//! reports the denial — so delegated fan-out cannot stampede one key.

use crate::key_arbitration::{arbiter, try_acquire, ArbiterLimits, Denial, KeyArbiter};

fn limits(per_key_concurrent: u32, per_key_requests: u32, global: u32) -> ArbiterLimits {
    ArbiterLimits {
        per_key_concurrent,
        per_key_requests,
        window_secs: 60,
        global_concurrent: global,
        quota: Vec::new(),
    }
}

#[test]
fn model_rate_arbitration_per_key_concurrency_bound() {
    let arb = KeyArbiter::new(limits(2, 100, 100));
    let a = arb.try_acquire("openai").expect("first admits");
    let b = arb.try_acquire("openai").expect("second admits");
    assert!(matches!(
        arb.try_acquire("openai"),
        Err(Denial::Concurrency)
    ));
    // A different key is unaffected.
    let c = arb.try_acquire("anthropic").expect("sibling key admits");
    drop(a);
    let d = arb.try_acquire("openai").expect("release frees a slot");
    drop((b, c, d));
    let status = arb.scope_status("openai");
    assert_eq!(status.in_flight, 0);
}

#[test]
fn model_rate_arbitration_per_key_rate_window() {
    let arb = KeyArbiter::new(limits(10, 2, 100));
    let a = arb.try_acquire("groq").expect("req 1");
    let b = arb.try_acquire("groq").expect("req 2");
    // Attempts count against the window even after permits release — a
    // failed-fast request still spent the provider's rate budget.
    drop((a, b));
    assert!(matches!(arb.try_acquire("groq"), Err(Denial::RateWindow)));
    assert_eq!(arb.scope_status("groq").window_used, 2);
}

#[test]
fn model_rate_arbitration_window_rolls_over() {
    let mut l = limits(10, 1, 100);
    l.window_secs = 0; // every acquire starts a fresh window
    let arb = KeyArbiter::new(l);
    let a = arb.try_acquire("x").expect("req 1");
    drop(a);
    let b = arb.try_acquire("x").expect("window rolled — req 2 admits");
    drop(b);
}

#[test]
fn model_rate_arbitration_global_bound_across_keys() {
    let arb = KeyArbiter::new(limits(10, 100, 1));
    let a = arb.try_acquire("key-a").expect("global slot 1");
    assert!(matches!(arb.try_acquire("key-b"), Err(Denial::Global)));
    drop(a);
    let b = arb.try_acquire("key-b").expect("freed global slot");
    drop(b);
}

#[test]
fn model_rate_arbitration_shared_process_arbiter() {
    // The process-global arbiter is the one production call sites use:
    // admission state is observable and released on drop.
    let scope = "mastery-shared-scope";
    let before = arbiter().scope_status(scope).in_flight;
    let permit = try_acquire(scope).expect("shared arbiter admits");
    assert_eq!(arbiter().scope_status(scope).in_flight, before + 1);
    drop(permit);
    assert_eq!(arbiter().scope_status(scope).in_flight, before);
}

#[test]
fn model_rate_arbitration_production_cascade_admits() {
    // The failover cascade in engines/runtime.rs — the production path
    // every `generate_reasoning*` entry point funnels through — must admit
    // each candidate against the arbiter before issuing the request.
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/engines/runtime.rs"
    ))
    .expect("runtime.rs readable");
    let cascade = src
        .split("fn try_providers")
        .nth(1)
        .expect("try_providers present");
    let acquire = cascade
        .find("crate::key_arbitration::try_acquire")
        .expect("cascade admits through the arbiter");
    let generate = cascade
        .find("provider.generate(prompt)")
        .expect("cascade issues provider.generate");
    assert!(
        acquire < generate,
        "arbiter must admit before the request is issued"
    );
    // The deny path skips the candidate (failover to the next provider),
    // like the provider_cooled check it sits beside.
    let cooled = cascade
        .find("provider_cooldown_until")
        .expect("cooldown check present");
    assert!(
        cooled < acquire,
        "arbitration sits on the production dispatch path beside cooldown"
    );
}

#[test]
fn model_rate_arbitration_probe_helpers_admit() {
    // Every one-shot paid probe also admits through the shared arbiter —
    // these run on the operator's keys and must not bypass the bounds.
    for file in [
        "/src/coding_models_ext.rs",
        "/src/frontier_ext.rs",
        "/src/open_weight_ext.rs",
        "/src/openrouter_ext.rs",
    ] {
        let src = std::fs::read_to_string(format!("{}{}", env!("CARGO_MANIFEST_DIR"), file))
            .unwrap_or_else(|_| panic!("{file} readable"));
        let acquire = src
            .find("crate::key_arbitration::try_acquire")
            .unwrap_or_else(|| panic!("{file} admits through the arbiter"));
        let generate = src
            .find("provider.generate")
            .unwrap_or_else(|| panic!("{file} issues a provider call"));
        assert!(acquire < generate, "{file}: admission precedes the call");
    }
}
