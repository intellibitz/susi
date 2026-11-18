//! Expected call/task cost priced against real catalog records, cache-hit
//! rates included (VC-202-002, T-DEEPSEEK-88).
//!
//! The deliverable is the number itself — expected cost of a call and of a
//! whole task from token counts and the cache hit rate the workload
//! achieves — consumed by routing: `Ranked.expected_cost_usd` rides the
//! brain's ranking surface, the `max_cost` ceiling in
//! `apply_cloud_constraints` drops candidates priced over the cap, and the
//! ladder's selected-reason names the figure.

use crate::engines::brain::TaskClass;
use crate::engines::cost;
use crate::engines::routing::InferenceRouter;
use crate::models::price_catalog::{PriceCatalog, PriceEntry};

fn entry(
    model_id: &str,
    input: f64,
    output: f64,
    hit: Option<f64>,
    miss: Option<f64>,
) -> PriceEntry {
    PriceEntry {
        model_id: model_id.to_string(),
        input_usd_per_1m: input,
        output_usd_per_1m: output,
        cache_hit_usd_per_1m: hit,
        cache_miss_usd_per_1m: miss,
    }
}

#[test]
fn expected_cost_with_cache_call_prices_hit_and_miss() {
    let mut cat = PriceCatalog::empty(1);
    // input $3/1M, output $15/1M, cache-hit $0.30/1M, cache-miss $3/1M.
    cat.insert(entry("sonnet", 3.0, 15.0, Some(0.30), Some(3.0)));

    // 1M prompt + 100K completion at 80% hits:
    // input = 0.8*0.30 + 0.2*3.0 = 0.84 → $0.84; output = 0.1*15 = $1.50.
    let cost = cat
        .expected_cost_usd("sonnet", 1_000_000, 100_000, 0.8)
        .expect("priced");
    assert!((cost - 2.34).abs() < 1e-6, "cost={cost}");

    // 0% hits prices every prompt token at the miss rate.
    let nocache = cat
        .expected_cost_usd("sonnet", 1_000_000, 100_000, 0.0)
        .expect("priced");
    assert!((nocache - 4.5).abs() < 1e-6, "nocache={nocache}");
    assert!(nocache > cost, "cache must make the same call cheaper");

    // No record → None, never an invented price.
    assert_eq!(cat.expected_cost_usd("unknown", 1, 1, 0.0), None);
}

#[test]
fn expected_cost_with_cache_absent_rates_bill_at_input_price() {
    let mut cat = PriceCatalog::empty(1);
    cat.insert(entry("plain", 3.0, 15.0, None, None));
    let at_zero = cat
        .expected_cost_usd("plain", 1_000_000, 0, 0.0)
        .expect("priced");
    let at_full = cat
        .expected_cost_usd("plain", 1_000_000, 0, 1.0)
        .expect("priced");
    assert_eq!(
        at_zero, at_full,
        "no cache rates → hit ratio changes nothing"
    );
    assert!((at_zero - 3.0).abs() < 1e-9);
}

#[test]
fn expected_cost_with_cache_task_profile_scales_by_class() {
    let mut cat = PriceCatalog::empty(1);
    cat.insert(entry("workhorse", 2.0, 8.0, Some(0.20), Some(2.0)));
    let reflex = cost::expected_task_cost_usd_in(Some(&cat), "x-workhorse", TaskClass::Reflex)
        .expect("priced");
    let reasoning =
        cost::expected_task_cost_usd_in(Some(&cat), "x-workhorse", TaskClass::Reasoning)
            .expect("priced");
    assert!(
        reasoning > reflex,
        "a reasoning task's token profile costs more than a reflex turn ({reasoning} > {reflex})"
    );
    // Reflex profile (512 in, 128 out, 0% hits) at $2/$8: 0.001024+0.001024.
    assert!((reflex - 0.002048).abs() < 1e-6, "reflex={reflex}");
}

#[test]
fn expected_cost_with_cache_composite_provider_names_resolve() {
    let mut cat = PriceCatalog::empty(1);
    cat.insert(entry("gpt-4o", 2.5, 10.0, None, None));
    cat.insert(entry("gpt-4o-mini", 0.15, 0.60, None, None));
    // "openai-gpt-4o-mini" contains both ids — longest match wins.
    let (p, c, h) = cost::task_token_profile(TaskClass::Chat);
    let mini =
        cost::expected_call_cost_usd_in(Some(&cat), "openai-gpt-4o-mini", p, c, h).expect("priced");
    let full =
        cost::expected_call_cost_usd_in(Some(&cat), "openai-gpt-4o", p, c, h).expect("priced");
    assert!(
        mini < full,
        "the mini price record must win the longer match"
    );
}

/// Install the test catalog under a process-private directory, replaced
/// atomically so a concurrent reader in this process (tests here take no env
/// lock) never sees an empty or partial file, and sibling nextest processes
/// never share the path at all. Entries cover every model_id the suite
/// prices.
fn install_catalog() -> PriceCatalog {
    let dir = std::env::temp_dir().join(format!("susi-prices-shared-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("model_prices.json");
    let mut cat = PriceCatalog::empty(1);
    cat.insert(entry("cheapmodel", 1.0, 4.0, Some(0.1), Some(1.0)));
    cat.insert(entry("premiummodel", 1.0, 10.0, Some(0.1), Some(1.0)));
    cat.insert(entry("budgetmodel", 0.01, 0.01, None, None));
    cat.insert(entry("pricymodel", 5.0, 50.0, None, None));
    cat.insert(entry("bargainmodel", 0.01, 0.01, None, None));
    cat.insert(entry("spendmodel", 0.01, 0.01, None, None));
    cat.insert(entry("doommodel", 5.0, 50.0, None, None));
    crate::susi_config::atomic_write_bytes(&path, cat.to_json().expect("catalog json").as_bytes())
        .expect("catalog file");
    // SAFETY: test-only env mutation; every caller sets the same path, whose
    // content is identical across writers and replaced atomically.
    unsafe {
        std::env::set_var("SUSI_PRICE_CATALOG_FILE", &path);
    }
    cat
}

#[test]
fn expected_cost_with_cache_ranked_carries_priced_task_cost() {
    let cat = install_catalog();

    let ranked = crate::engines::brain::rank(&["acme-cheapmodel".to_string()], TaskClass::Chat);
    let r = ranked.first().expect("one ranked provider");
    let usd = r
        .expected_cost_usd
        .expect("a priced provider carries its expected task cost");
    let (p, c, h) = cost::task_token_profile(TaskClass::Chat);
    let want = cat
        .expected_cost_usd("cheapmodel", p, c, h)
        .expect("priced");
    assert!(
        (usd - want).abs() < 1e-9,
        "Ranked.expected_cost_usd must price the same task profile ({usd} vs {want})"
    );

    // An unpriced provider stays None — the coarse tier covers it.
    let unpriced = crate::engines::brain::rank(&["acme-unpriced".to_string()], TaskClass::Chat);
    assert_eq!(unpriced[0].expected_cost_usd, None);
}

#[test]
fn expected_cost_with_cache_max_cost_drops_over_cap_priced_provider() {
    // Reasoning task profile (49K in @75% hits, 3K out): "premiummodel" is
    // priced ~$0.05 — a $0.01 cap drops it even though name rules call it
    // Low tier; "budgetmodel" prices under the cap and stays. The filter
    // itself is exercised directly: `plan_placement_for` prefers the
    // process-global registry over the passed list, and other tests
    // legitimately register providers there — a deterministic candidate
    // list keeps this test about the ceiling, not about who else ran.
    let _cat = install_catalog();
    let mut clouds = vec![
        "vendor-premiummodel".to_string(),
        "vendor-budgetmodel".to_string(),
    ];
    InferenceRouter::apply_cloud_constraints(&mut clouds, Some("reasoning"), Some(0.01));
    assert!(
        !clouds.iter().any(|n| n.contains("premiummodel")),
        "a provider priced over the max_cost ceiling is dropped: {clouds:?}"
    );
    assert!(
        clouds.iter().any(|n| n.contains("budgetmodel")),
        "a provider priced under the cap stays: {clouds:?}"
    );
}

#[test]
fn expected_cost_with_cache_production_wiring() {
    // The cost must feed routing on the production path — a catalog nothing
    // consumes is scaffold, not delivery.
    let routing_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/engines/routing.rs"
    ))
    .expect("routing.rs readable");
    assert!(
        routing_src.contains("expected_task_cost_usd_in"),
        "the max_cost ceiling must price candidates from the catalog"
    );
    let brain_src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engines/brain.rs"))
            .expect("brain.rs readable");
    assert!(
        brain_src.contains("expected_cost_usd"),
        "Ranked must carry the priced task cost for ranking surfaces"
    );
    let runtime_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/engines/runtime.rs"
    ))
    .expect("runtime.rs readable");
    assert!(
        runtime_src.contains("expected_task_cost_usd"),
        "the production cascade must name the expected cost in the ladder trace"
    );
}
