use susi_adapters_llm::inference_wire::TokenUsage as WireUsage;
use susi_gawd::telemetry::{InferenceTelemetry, TokenUsage};

#[test]
fn telemetry_exposition_preserves_the_complete_public_document() {
    let telemetry = InferenceTelemetry::new();
    telemetry.record_call("beta", &TokenUsage::with_cache(10, 5, 4), 20, true);
    telemetry.record_call("alpha", &TokenUsage::new(2, 3), 5, false);
    assert_eq!(
        telemetry.render_prometheus(),
        include_str!("fixtures/telemetry.prom")
    );
}

#[test]
fn telemetry_const_construction_and_conversions_preserve_saturation() {
    const SATURATED: TokenUsage = TokenUsage::new(u64::MAX, 1);
    assert_eq!(SATURATED.total_tokens, u64::MAX);
    assert_eq!(SATURATED.cached_tokens, 0);
    let manually_reported = TokenUsage {
        prompt_tokens: 10,
        completion_tokens: 5,
        cached_tokens: 4,
        total_tokens: 99,
    };
    let borrowed: WireUsage = (&manually_reported).into();
    let owned: WireUsage = manually_reported.into();
    assert_eq!(borrowed, WireUsage::with_cache(10, 5, 4));
    assert_eq!(owned, borrowed);
}
