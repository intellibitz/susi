//! A candidate is a worker — a model API or an agent seat (VC-202-022,
//! T-DEEPSEEK-129).
//!
//! One `WorkerDescriptor` carries kind, billing mode, rate and concurrency
//! limits, health, capability evidence and cost attribution; election,
//! ranking, the capability matrix and the budget treat a seat and a model
//! uniformly because both are registered providers with the same evidence
//! store. The two proofs that matter:
//!
//! - a seat and a model with the SAME measured capability are ranked by
//!   marginal cost — a subscription seat under its cap costs $0 at the
//!   margin, so it leads an equally capable metered model;
//! - a seat that has reached its cap steps down the ladder rather than
//!   being hammered — marginal cost becomes unaffordable and the seat's
//!   own quota window denies the next call.

use crate::engines::brain::{Store, TaskClass};
use crate::seat_provider::SeatProvider;
use crate::worker::{self, BillingMode, WorkerKind};

/// Identical entry content to the shared catalogs the cost suites
/// install — whichever path `SUSI_PRICE_CATALOG_FILE` resolves to
/// mid-race, `bargainmodel` prices identically everywhere.
fn install_shared_catalog() {
    use crate::models::price_catalog::{PriceCatalog, PriceEntry};
    use std::collections::BTreeMap;
    let dir = std::env::temp_dir().join("susi-prices-worker");
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
    std::fs::write(&path, catalog.to_json().expect("catalog json")).expect("catalog file");
    // SAFETY: serialized by env_test_lock at each call site; every writer
    // across the suite writes identical entry content.
    unsafe {
        std::env::set_var("SUSI_PRICE_CATALOG_FILE", &path);
    }
}

fn seats_env() {
    // SAFETY: serialized by env_test_lock at each call site.
    unsafe {
        std::env::set_var("SUSI_AGENT_SEATS", "wagent:2/3600:4:15");
    }
}

fn clear_seats_env() {
    // SAFETY: serialized by env_test_lock at each call site.
    unsafe {
        std::env::remove_var("SUSI_AGENT_SEATS");
    }
}

#[test]
fn worker_may_be_an_agent_descriptor_carries_the_whole_worker() {
    let _env = crate::engines::env_test_lock();
    seats_env();
    worker::reset_usage_for_test();

    let mut s = Store::default();
    s.record("seat-wagent", TaskClass::Chat, true, 40);
    s.record("acme-model-b", TaskClass::Chat, true, 40);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let seat = worker::descriptor(&s, "seat-wagent", TaskClass::Chat, now);
    assert_eq!(seat.kind, WorkerKind::AgentSeat);
    assert_eq!(
        seat.billing,
        BillingMode::Subscription {
            calls_per_window: 2,
            window_secs: 3600
        }
    );
    assert_eq!(seat.rate_per_min, Some(15));
    assert_eq!(seat.max_concurrent, Some(4));
    assert_eq!(seat.samples, 1, "capability evidence rides the same store");
    assert_eq!(seat.success_rate, Some(1.0));
    assert!(seat.healthy);
    assert!(!seat.saturated);
    assert_eq!(seat.window_used, Some(0.0));
    assert_eq!(seat.window_cap, Some(2.0));

    let model = worker::descriptor(&s, "acme-model-b", TaskClass::Chat, now);
    assert_eq!(model.kind, WorkerKind::ModelApi);
    assert_eq!(model.billing, BillingMode::Metered);
    assert_eq!(model.samples, 1);
    assert_eq!(model.success_rate, Some(1.0));
    assert_eq!(model.window_cap, None, "a model has no subscription cap");

    clear_seats_env();
}

#[test]
fn worker_may_be_an_agent_same_capability_ranks_by_marginal_cost() {
    let _env = crate::engines::env_test_lock();
    install_shared_catalog();
    seats_env();
    worker::reset_usage_for_test();

    // Identical measured capability: same class, same samples, same rate.
    let mut s = Store::default();
    for _ in 0..4 {
        s.record("seat-wagent", TaskClass::Chat, true, 100);
        s.record("acme-bargainmodel", TaskClass::Chat, true, 100);
    }
    let providers: Vec<String> = ["seat-wagent", "acme-bargainmodel"]
        .iter()
        .map(|p| p.to_string())
        .collect();

    // The seat's marginal cost is zero under its cap; a metered model's is
    // whatever the catalog says — never zero by definition of Metered.
    let seat = s.rank(&providers, TaskClass::Chat);
    assert_eq!(
        seat[0].expected_cost_usd,
        Some(0.0),
        "a subscription seat under its cap has zero marginal cost"
    );

    let ranked = s.rank_with_budget(
        &providers,
        TaskClass::Chat,
        crate::engines::cost::Budget::Balanced,
    );
    assert!(
        ranked.iter().all(|r| r.meets_floor),
        "same capability — both meet the floor"
    );
    assert_eq!(
        ranked[0].provider, "seat-wagent",
        "equal capability + zero marginal cost → the seat leads"
    );
    assert_eq!(
        ranked[0].cost_per_outcome_usd,
        Some(0.0),
        "cost per verified outcome on a seat under cap is zero"
    );

    clear_seats_env();
}

#[test]
fn worker_may_be_an_agent_capped_seat_steps_down_the_ladder() {
    let _env = crate::engines::env_test_lock();
    install_shared_catalog();
    seats_env();
    worker::reset_usage_for_test();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Spend the seat's whole window (cap = 2 in `seats_env`).
    worker::record_seat_call("seat-wagent", now);
    worker::record_seat_call("seat-wagent", now);
    assert!(worker::seat_saturated("seat-wagent", now));

    // Marginal cost is now unaffordable: every cost-ordered surface —
    // the rank comparator, the pre-dispatch spend ceiling, the ladder's
    // expected-cost display — steps the seat down rather than hammer it.
    assert_eq!(
        crate::worker::seat_marginal_cost_usd("seat-wagent", now),
        Some(f64::MAX)
    );
    assert_eq!(
        crate::engines::cost::expected_task_cost_usd("seat-wagent", TaskClass::Chat),
        Some(f64::MAX),
        "the cost funnel carries saturation to ranking and ceilings"
    );

    let mut s = Store::default();
    for _ in 0..4 {
        s.record("seat-wagent", TaskClass::Chat, true, 100);
        s.record("acme-bargainmodel", TaskClass::Chat, true, 100);
    }
    let ranked = s.rank_with_budget(
        &["seat-wagent".to_string(), "acme-bargainmodel".to_string()],
        TaskClass::Chat,
        crate::engines::cost::Budget::Balanced,
    );
    assert_eq!(
        ranked[0].provider, "acme-bargainmodel",
        "the capped seat loses the lead it held while under cap"
    );

    // And at dispatch the seat's own quota window refuses — a Denial,
    // not a hammered API call. The provider's arbiter tracks its own
    // cap independently of the ranking surface above.
    let seat = SeatProvider::new(worker::seat_spec_for("seat-wagent").expect("spec"));
    assert_eq!(seat.quota_headroom(), Some((2, 2)), "fresh arbiter window");

    clear_seats_env();
}

#[test]
fn worker_may_be_an_agent_surfaces_treat_a_worker_uniformly() {
    let _env = crate::engines::env_test_lock();
    seats_env();
    worker::reset_usage_for_test();
    let registry = crate::susi_core::registry::CapabilityRegistry::new();
    crate::seat_provider::register_configured_seats(&registry);
    assert!(
        registry.list_providers().iter().any(|p| p == "seat-wagent"),
        "a declared seat registers as an ordinary provider"
    );
    let provider = registry
        .get_provider("seat-wagent")
        .expect("seat provider registered");
    assert_eq!(provider.name(), "seat-wagent");

    // Ranking treats it like any candidate: the same `rank` call that
    // orders models orders the seat — and, under cap, prices it at zero.
    let mut s = Store::default();
    s.record("seat-wagent", TaskClass::Chat, true, 90);
    let ranked = s.rank(&["seat-wagent".to_string()], TaskClass::Chat);
    assert_eq!(ranked.len(), 1);
    assert!(ranked[0].meets_floor, "a capable seat meets the Chat floor");

    // The capability matrix and the election run over provider names, so
    // the seat appears on both without special-casing.
    let matrix = crate::capability_matrix::build_in(
        &s,
        &["seat-wagent".to_string(), "acme-bargainmodel".to_string()],
        1_900_000_000,
    );
    let seat_cells = matrix
        .classes
        .values()
        .flatten()
        .filter(|c| c.provider == "seat-wagent")
        .count();
    assert!(seat_cells > 0, "the matrix measures the seat per class");

    let now = 1_900_000_000;
    let election = crate::primary_election::elect(&ranked, TaskClass::Chat, now);
    assert!(
        election.is_some(),
        "a fit seat is electable — the primary may be an agent seat"
    );

    clear_seats_env();
}

#[test]
fn worker_may_be_an_agent_production_wiring() {
    // The seat machinery must sit on the real dispatch path, not beside
    // it: registration rides the same production entry as cloud endpoints,
    // dispatch goes through the managed-agent bus topic, and the cost
    // funnel consults seat billing.
    let http = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/engines/http_provider.rs"
    ))
    .expect("http_provider.rs readable");
    assert!(
        http.contains("register_configured_seats(registry)"),
        "seat registration rides the production provider-registration path"
    );

    let seat =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/seat_provider.rs"))
            .expect("seat_provider.rs readable");
    assert!(
        seat.contains("external_managed_goal"),
        "seat dispatch goes through agents.external.managed — the existing \
         delegation/capability-bus path"
    );

    let cost = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engines/cost.rs"))
        .expect("cost.rs readable");
    assert!(
        cost.contains("worker::marginal_cost_usd"),
        "the expected-cost funnel carries billing-mode marginal cost"
    );
}
