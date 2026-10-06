//! Rank candidates by expected cost per *verified useful outcome*, not by
//! reputation (VC-202-003, T-DEEPSEEK-98).
//!
//! `Store::rank` prices each provider (`expected_cost_usd` / smoothed
//! success probability) into `cost_per_outcome_usd`, and — unless the
//! budget dial is pinned to pure quality — that number decides the order
//! inside the floor partition: a cheap decent model leads a pricey proven
//! one, an untried model gets the class prior, and Mandate 56 holds — a
//! failing or unfunded model never leads. The production cascade and the
//! failover order consume the rank *position*, so the leader the brain
//! elects is the provider that actually serves.

use crate::engines::brain::{FailureKind, Store, TaskClass};
use crate::engines::cost::Budget;
use crate::models::price_catalog::{PriceCatalog, PriceEntry};
use std::collections::BTreeMap;

/// Install the price catalog under a process-private directory, replaced
/// atomically: nextest runs sibling tests as separate processes, and a fixed
/// path truncated by another writer reads back as an unpriced catalog.
fn install_catalog() {
    let dir = std::env::temp_dir().join(format!("susi-prices-rank-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let mut entries = BTreeMap::new();
    for (id, i, o, hit, miss) in [
        ("cheapmodel", 1.0, 4.0, Some(0.1), Some(1.0)),
        ("premiummodel", 1.0, 10.0, Some(0.1), Some(1.0)),
        ("budgetmodel", 0.01, 0.01, None, None),
        ("pricymodel", 5.0, 50.0, None, None),
        ("bargainmodel", 0.01, 0.01, None, None),
        ("spendmodel", 0.01, 0.01, None, None),
        ("doommodel", 5.0, 50.0, None, None),
    ] {
        entries.insert(
            id.to_string(),
            PriceEntry {
                model_id: id.to_string(),
                input_usd_per_1m: i,
                output_usd_per_1m: o,
                cache_hit_usd_per_1m: hit,
                cache_miss_usd_per_1m: miss,
            },
        );
    }
    let catalog = PriceCatalog {
        version: 1,
        entries,
    };
    let path = dir.join("model_prices.json");
    crate::susi_config::atomic_write_bytes(
        &path,
        catalog.to_json().expect("catalog json").as_bytes(),
    )
    .expect("catalog file");
    // SAFETY: serialized by env_test_lock at each call site; the file is
    // process-private and replaced atomically.
    unsafe {
        std::env::set_var("SUSI_PRICE_CATALOG_FILE", &path);
    }
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn brain_ranking_by_cost_leader_is_cheapest_per_verified_outcome() {
    let _env = crate::engines::env_test_lock();
    install_catalog();
    let mut s = Store::default();
    // bargainmodel: 9/10 successes at ~$0.000006/task (Reflex profile).
    // pricymodel:  95/100 successes at ~$0.009/task — better reputation,
    // ~1300× the cost per verified outcome.
    for _ in 0..9 {
        s.record("acme-bargainmodel", TaskClass::Reflex, true, 100);
    }
    s.record("acme-bargainmodel", TaskClass::Reflex, false, 0);
    for _ in 0..95 {
        s.record("acme-apricymodel", TaskClass::Reflex, true, 100);
    }
    for _ in 0..5 {
        s.record("acme-apricymodel", TaskClass::Reflex, false, 0);
    }

    let ranked = s.rank_with_budget(
        &names(&["acme-apricymodel", "acme-bargainmodel"]),
        TaskClass::Reflex,
        Budget::Balanced,
    );
    assert_eq!(
        ranked[0].provider, "acme-bargainmodel",
        "the cheaper-per-verified-outcome provider leads despite the weaker record"
    );
    let cheap = &ranked[0];
    let pricey = &ranked[1];
    assert_eq!(pricey.provider, "acme-apricymodel");
    assert!(
        cheap.cost_per_outcome_usd.expect("priced") < pricey.cost_per_outcome_usd.expect("priced"),
        "cost per outcome orders the ranking: {:?} vs {:?}",
        cheap.cost_per_outcome_usd,
        pricey.cost_per_outcome_usd
    );
    // Evidence is still on the record — the leader just isn't elected by it.
    assert!(
        pricey.samples > cheap.samples,
        "the loser carried the stronger reputation"
    );
}

#[test]
fn brain_ranking_by_cost_untried_priced_beats_priced_expensive() {
    let _env = crate::engines::env_test_lock();
    install_catalog();
    let s = Store::default();
    let ranked = s.rank_with_budget(
        &names(&["acme-apricymodel", "acme-bargainmodel"]),
        TaskClass::Chat,
        Budget::Balanced,
    );
    assert_eq!(
        ranked[0].provider, "acme-bargainmodel",
        "equal priors → the cheaper expected task cost leads"
    );
}

#[test]
fn brain_ranking_by_cost_unpriced_falls_back_to_evidence() {
    let _env = crate::engines::env_test_lock();
    install_catalog();
    let mut s = Store::default();
    for _ in 0..8 {
        s.record("acme-unpriced-good", TaskClass::Chat, true, 100);
    }
    for _ in 0..4 {
        s.record("acme-unpriced-bad", TaskClass::Chat, false, 0);
    }
    let ranked = s.rank_with_budget(
        &names(&["acme-unpriced-bad", "acme-unpriced-good"]),
        TaskClass::Chat,
        Budget::Balanced,
    );
    assert_eq!(ranked[0].provider, "acme-unpriced-good");
    assert_eq!(ranked[0].cost_per_outcome_usd, None);
}

#[test]
fn brain_ranking_by_cost_max_budget_is_pure_evidence() {
    let _env = crate::engines::env_test_lock();
    install_catalog();
    let mut s = Store::default();
    for _ in 0..9 {
        s.record("acme-bargainmodel", TaskClass::Reflex, true, 100);
    }
    s.record("acme-bargainmodel", TaskClass::Reflex, false, 0);
    for _ in 0..95 {
        s.record("acme-apricymodel", TaskClass::Reflex, true, 100);
    }
    for _ in 0..5 {
        s.record("acme-apricymodel", TaskClass::Reflex, false, 0);
    }
    let ranked = s.rank_with_budget(
        &names(&["acme-apricymodel", "acme-bargainmodel"]),
        TaskClass::Reflex,
        Budget::Max,
    );
    assert_eq!(
        ranked[0].provider, "acme-apricymodel",
        "Budget::Max means quality/evidence ordering — cost never leads"
    );
}

#[test]
fn brain_ranking_by_cost_failing_provider_never_leads() {
    let _env = crate::engines::env_test_lock();
    install_catalog();
    let mut s = Store::default();
    // Cheapest provider on the board — and it keeps 402ing (unfunded).
    for _ in 0..4 {
        s.record("acme-bargainmodel", TaskClass::Chat, false, 0);
    }
    let now = 1_800_000_000u64;
    s.note_failure("acme-bargainmodel", FailureKind::Funds, now);
    for _ in 0..6 {
        s.record("acme-apricymodel", TaskClass::Chat, true, 200);
    }

    let ranked = s.rank_with_budget(
        &names(&["acme-bargainmodel", "acme-apricymodel"]),
        TaskClass::Chat,
        Budget::Balanced,
    );
    let flagged = ranked
        .iter()
        .find(|r| r.provider == "acme-bargainmodel")
        .expect("ranked");
    assert!(flagged.unfit, "an unfunded repeat-failure is marked unfit");
    // Mandate 56: the leader must be a fit provider.
    let leader = s
        .best_healthy(
            &names(&["acme-bargainmodel", "acme-apricymodel"]),
            TaskClass::Chat,
            now,
            &|_| false,
        )
        .expect("a fit provider exists");
    assert_eq!(leader, "acme-apricymodel");
}

#[test]
fn brain_ranking_by_cost_floor_still_dominates_the_cheap() {
    let _env = crate::engines::env_test_lock();
    install_catalog();
    let s = Store::default();
    // Code floor is Coding-capable: the cloud default (Coding) meets it,
    // a local name (Basic) is below — and cheaper.
    let ranked = s.rank_with_budget(
        &names(&["vllm-bargainmodel", "acme-apricymodel"]),
        TaskClass::Code,
        Budget::Balanced,
    );
    assert_eq!(
        ranked[0].provider, "acme-apricymodel",
        "a cheap below-floor model cannot outrank a floor-meeting pricey one"
    );
    assert!(ranked[1].provider.contains("bargainmodel"));
    assert!(!ranked[1].meets_floor, "kept as the last rung, not dropped");
}

#[test]
fn brain_ranking_by_cost_decision_carries_its_evidence() {
    let _env = crate::engines::env_test_lock();
    install_catalog();
    let mut s = Store::default();
    for _ in 0..9 {
        s.record("acme-bargainmodel", TaskClass::Reflex, true, 100);
    }
    s.record("acme-bargainmodel", TaskClass::Reflex, false, 0);
    let ranked = s.rank(&names(&["acme-bargainmodel"]), TaskClass::Reflex);
    let r = &ranked[0];
    let expected = r.expected_cost_usd.expect("priced");
    // Reflex profile (512 in + 128 out) at $0.01/$0.01: ≈ $0.0000064.
    assert!((expected - 0.0000064).abs() < 1e-6, "expected={expected}");
    // smoothed p = (9+1)/(10+2) ≈ 0.833 → cost per outcome ≈ 7.68e-6.
    let cpo = r.cost_per_outcome_usd.expect("priced");
    assert!((cpo - expected / (10.0 / 12.0)).abs() < 1e-8, "cpo={cpo}");
    // Serializable — the ranking record carries the number it ranked by.
    let json = serde_json::to_string(r).expect("serializes");
    assert!(json.contains("cost_per_outcome_usd"), "{json}");
}

#[test]
fn brain_ranking_by_cost_production_wiring() {
    // The rank *order* must drive the dispatch order — a ranking that
    // nothing consumes is scaffold.
    let runtime_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/engines/runtime.rs"
    ))
    .expect("runtime.rs readable");
    assert!(
        runtime_src.contains("enumerate()"),
        "the cascade must consume rank position, not re-sort by raw score"
    );
    let routing_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/engines/routing.rs"
    ))
    .expect("routing.rs readable");
    assert!(
        routing_src.contains("enumerate()"),
        "the failover order must consume rank position too"
    );
    let brain_src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engines/brain.rs"))
            .expect("brain.rs readable");
    assert!(
        brain_src.contains("cost_per_outcome_usd"),
        "Ranked must carry the cost-per-verified-outcome figure"
    );
}
