//! Rank candidates by the expected marginal cost of the next call plus
//! the scarcity of their remaining cap (VC-202-021, T-DEEPSEEK-120).
//!
//! The production ranking — `Store::rank_with_quota`, reached by
//! `Store::rank`/`rank_with_budget` and by the failover order — prices
//! the NEXT call under the provider's billing mode (metered sticker,
//! subscription/free-tier zero-under-cap, prepaid remaining stock), then
//! scales the cost per verified outcome by a scarcity penalty on the
//! tightest remaining cap fraction. The proofs that matter:
//!
//! - a scarce pay-as-you-go key steps below an abundant twin but still
//!   beats a far pricier abundant one — cost stays primary, scarcity
//!   adjusts gradually rather than gating;
//! - an exhausted cap can never lead — not on evidence, not on
//!   Budget::Max;
//! - the billing mode sets the marginal number: subscription and free
//!   tier are zero under cap, prepaid is bounded by remaining stock, and
//!   `record_spend` (fed by `spend_tracker::record` on every dispatch)
//!   depletes the declared cap.

use crate::engines::brain::{Store, TaskClass};
use crate::engines::cost::Budget;

/// Install the shared price catalog under a process-private directory,
/// replaced atomically: nextest runs sibling tests as separate processes, and
/// a fixed path truncated by another writer reads back as an unpriced catalog.
fn install_shared_catalog(env: &mut susi_paths::test_env::EnvGuard) {
    use crate::models::price_catalog::{PriceCatalog, PriceEntry};
    use std::collections::BTreeMap;
    let dir = std::env::temp_dir().join(format!("susi-prices-marginal-{}", std::process::id()));
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
    env.set("SUSI_PRICE_CATALOG_FILE", &path);
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn names(providers: &[&str]) -> Vec<String> {
    providers.iter().map(|p| p.to_string()).collect()
}

/// Equal evidence on every candidate, so the only thing left to compare
/// is the cost axis.
fn even_store(providers: &[&str]) -> Store {
    let mut s = Store::default();
    for p in providers {
        for _ in 0..4 {
            s.record(p, TaskClass::Chat, true, 100);
        }
    }
    s
}

#[test]
fn ranking_by_marginal_cost_scarcity_demotes_but_cost_stays_primary() {
    let _lock = crate::engines::env_test_lock();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.remove("SUSI_BILLING");
    env.remove("SUSI_AGENT_SEATS");
    install_shared_catalog(&mut env);
    crate::worker::reset_usage_for_test();

    // spendmodel and bargainmodel share a sticker price; pricymodel is
    // ~1200x pricier. spendmodel's cap is nearly spent.
    let providers = names(&["acme-pricymodel", "acme-spendmodel", "acme-bargainmodel"]);
    let s = even_store(&["acme-pricymodel", "acme-spendmodel", "acme-bargainmodel"]);
    let fraction = |name: &str| -> Option<f64> {
        match name {
            "acme-spendmodel" => Some(0.05), // 5% of the cap left
            "acme-bargainmodel" => Some(1.0),
            "acme-pricymodel" => Some(1.0),
            _ => None,
        }
    };
    let ranked = s.rank_with_quota(&providers, TaskClass::Chat, Budget::Balanced, &fraction);

    assert_eq!(
        ranked[0].provider, "acme-bargainmodel",
        "abundant twin of the same sticker price leads"
    );
    assert_eq!(
        ranked[1].provider, "acme-spendmodel",
        "scarce cheap still beats far pricier abundant — cost stays primary"
    );
    assert_eq!(ranked[2].provider, "acme-pricymodel");

    let scarce = &ranked[1];
    assert_eq!(scarce.quota_remaining, Some(0.05));
    let base = scarce.cost_per_outcome_usd.expect("priced");
    let eff = scarce.effective_cost_usd.expect("effective");
    // penalty = (1 - 0.05)/0.05 = 19 → effective = base * 20.
    assert!(
        (eff - base * 20.0).abs() < base,
        "scarcity scales the marginal cost: base {base} effective {eff}"
    );
    assert!(
        eff < ranked[2].effective_cost_usd.unwrap_or(f64::MAX),
        "a cheaper scarce provider still beats a pricier abundant one"
    );
}

#[test]
fn ranking_by_marginal_cost_exhausted_never_leads() {
    let _lock = crate::engines::env_test_lock();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.remove("SUSI_BILLING");
    env.remove("SUSI_AGENT_SEATS");
    crate::worker::reset_usage_for_test();

    // Unpriced candidates: capability evidence is the only score — and an
    // exhausted cap still loses to it. Give the spent provider MORE
    // evidence to prove it cannot win on raw capability.
    let mut s = Store::default();
    for _ in 0..8 {
        s.record("acme-noprice-spent", TaskClass::Chat, true, 10);
    }
    s.record("acme-noprice-fresh", TaskClass::Chat, true, 10);
    let providers = names(&["acme-noprice-spent", "acme-noprice-fresh"]);
    let fraction = |name: &str| -> Option<f64> {
        if name == "acme-noprice-spent" {
            Some(0.0)
        } else {
            None
        }
    };
    for budget in [Budget::Balanced, Budget::Max] {
        let ranked = s.rank_with_quota(&providers, TaskClass::Chat, budget, &fraction);
        assert_eq!(
            ranked[0].provider, "acme-noprice-fresh",
            "a spent cap cannot lead on raw capability ({budget:?})"
        );
        assert_eq!(ranked[1].quota_remaining, Some(0.0));
        assert_eq!(
            ranked[1].effective_cost_usd,
            Some(f64::MAX),
            "a spent cap prices as unaffordable"
        );
    }
}

#[test]
fn ranking_by_marginal_cost_billing_modes_set_the_marginal() {
    let _lock = crate::engines::env_test_lock();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    install_shared_catalog(&mut env);
    env.remove("SUSI_AGENT_SEATS");
    // A subscription plan, a free tier and a prepaid stock declared for
    // ordinary provider keys — no seat involved.
    env.set(
        "SUSI_BILLING",
        "acme-bargainmodel:subscription:3/3600,acme-spendmodel:free:3/3600,acme-pricymodel:prepaid:0.0005",
    );
    crate::worker::reset_usage_for_test();

    let providers = names(&[
        "acme-budgetmodel",  // metered, cheapest sticker
        "acme-spendmodel",   // free tier under cap
        "acme-bargainmodel", // subscription under cap
        "acme-pricymodel",   // prepaid stock too small for one call
    ]);
    let s = even_store(&[
        "acme-budgetmodel",
        "acme-spendmodel",
        "acme-bargainmodel",
        "acme-pricymodel",
    ]);
    let uncapped = |_: &str| -> Option<f64> { None };
    let ranked = s.rank_with_quota(&providers, TaskClass::Chat, Budget::Balanced, &uncapped);

    // Zero marginal cost under the cap beats any metered sticker — the
    // subscription and the free tier tie at $0 and keep the caller's order.
    let zero_marginal: Vec<&crate::engines::brain::Ranked> = ranked[..2].iter().collect::<Vec<_>>();
    let modes: Vec<&'static str> = ranked[..2].iter().map(|r| r.billing).collect();
    assert!(
        modes.contains(&"subscription") && modes.contains(&"free-tier"),
        "subscription and free tier both lead at zero marginal: {modes:?}"
    );
    assert!(
        zero_marginal
            .iter()
            .all(|r| r.expected_cost_usd == Some(0.0)),
        "both capped modes price the next call at zero"
    );
    assert_eq!(ranked[2].provider, "acme-budgetmodel");
    assert_eq!(ranked[2].billing, "metered");
    // The prepaid stock ($0.0005) cannot cover a ~$0.03 call → saturated.
    assert_eq!(ranked[3].provider, "acme-pricymodel");
    assert_eq!(ranked[3].billing, "prepaid");
    assert!(
        ranked[3].effective_cost_usd.is_some_and(|c| c >= f64::MAX),
        "a prepaid balance too small for the next call is unaffordable"
    );
}

#[test]
fn ranking_by_marginal_cost_spend_depletes_declared_caps() {
    let _lock = crate::engines::env_test_lock();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    install_shared_catalog(&mut env);
    env.remove("SUSI_AGENT_SEATS");
    env.set("SUSI_BILLING", "acme-budgetmodel:free:2/3600");
    crate::worker::reset_usage_for_test();

    // Two dispatches attributed by spend_tracker::record burn the whole
    // free-tier window — depletion rides real dispatch, not a side
    // channel nobody calls.
    let t = now();
    crate::worker::record_spend("acme-budgetmodel", 0.0, t);
    assert_eq!(
        crate::worker::remaining_fraction("acme-budgetmodel", t),
        Some(0.5)
    );
    crate::worker::record_spend("acme-budgetmodel", 0.0, t);
    assert_eq!(
        crate::worker::remaining_fraction("acme-budgetmodel", t),
        Some(0.0)
    );
    assert!(crate::worker::saturated("acme-budgetmodel", t));
    assert_eq!(
        crate::engines::cost::expected_task_cost_usd("acme-budgetmodel", TaskClass::Chat),
        Some(f64::MAX),
        "a spent declared cap prices as unaffordable on the cost funnel"
    );

    // And the next rank demotes it behind an uncapped peer even though it
    // carried the same evidence and a cheaper sticker.
    let mut s = Store::default();
    for _ in 0..8 {
        s.record("acme-budgetmodel", TaskClass::Chat, true, 50);
    }
    s.record("acme-bargainmodel", TaskClass::Chat, true, 50);
    let providers = names(&["acme-budgetmodel", "acme-bargainmodel"]);
    let ranked = s.rank_with_quota(
        &providers,
        TaskClass::Chat,
        Budget::Balanced,
        &crate::worker::remaining_fraction_unified,
    );
    assert_eq!(
        ranked[0].provider, "acme-bargainmodel",
        "the spent free tier steps down behind the uncapped metered model"
    );
    assert_eq!(ranked[1].quota_remaining, Some(0.0));
}

#[test]
fn ranking_by_marginal_cost_prepaid_depletion() {
    let _lock = crate::engines::env_test_lock();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    install_shared_catalog(&mut env);
    env.remove("SUSI_AGENT_SEATS");
    env.set("SUSI_BILLING", "acme-budgetmodel:prepaid:0.0001");
    crate::worker::reset_usage_for_test();

    let t = now();
    // A Chat call on budgetmodel expects ~(2048*0.01 + 384*0.01)/1e6 usd.
    let expected =
        crate::engines::cost::expected_task_cost_usd("acme-budgetmodel", TaskClass::Chat)
            .expect("priced");
    assert!(expected > 0.0 && expected < 0.0001, "expected {expected}");

    // Spend half the stock: the remaining half still covers the call.
    crate::worker::record_spend("acme-budgetmodel", 0.00005, t);
    assert_eq!(
        crate::engines::cost::expected_task_cost_usd("acme-budgetmodel", TaskClass::Chat),
        Some(expected),
        "the call fits the remaining stock — the charge is the expected cost"
    );
    assert!(
        crate::worker::remaining_fraction("acme-budgetmodel", t) == Some(0.5),
        "the balance is a quota: half spent is half remaining"
    );

    // Drain it: no remaining stock, no affordable next call.
    crate::worker::record_spend("acme-budgetmodel", 0.00005, t);
    assert!(crate::worker::saturated("acme-budgetmodel", t));
    assert_eq!(
        crate::engines::cost::expected_task_cost_usd("acme-budgetmodel", TaskClass::Chat),
        Some(f64::MAX)
    );
}

#[test]
fn ranking_by_marginal_cost_production_wiring() {
    // The marginal-cost-plus-scarcity ordering must live on the real
    // ranking path, not beside it.
    let brain =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engines/brain.rs"))
            .expect("brain.rs readable");
    for needle in ["rank_with_quota", "effective_cost_usd", "quota_exhausted"] {
        assert!(
            brain.contains(needle),
            "Store::rank must order by scarcity-adjusted marginal cost ({needle})"
        );
    }
    assert!(
        brain.contains("remaining_fraction_unified"),
        "the production rank consults the unified cap view (worker windows, \
         prepaid balances, arbiter quota)"
    );

    let cost = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engines/cost.rs"))
        .expect("cost.rs readable");
    assert!(
        cost.contains("worker::marginal_cost_usd"),
        "the expected-cost funnel prices the next call under the billing mode"
    );

    let spend =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/spend_tracker.rs"))
            .expect("spend_tracker.rs readable");
    assert!(
        spend.contains("worker::record_spend"),
        "every attributed dispatch depletes the declared cap"
    );

    let routing = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/engines/routing.rs"
    ))
    .expect("routing.rs readable");
    assert!(
        routing.contains("rank_with_quota"),
        "the failover order feeds quota headroom into the rank's scarcity signal"
    );
}
